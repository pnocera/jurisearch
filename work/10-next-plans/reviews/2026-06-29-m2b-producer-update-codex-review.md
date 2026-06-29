# M2-B Producer Update Review

## Findings

### BLOCKER: publish failure after catalog insert leaves the producer stuck on an unpublished package

The no-partial / resumability gate is not met for failures between incremental build/cataloging and filesystem publish. `build_incremental_inner` writes the signed artifact and immediately inserts the new `package_catalog` row with `status: "built"` before returning to `producer_cycle` (`crates/jurisearch-package-build/src/incremental.rs:500`, `crates/jurisearch-package-build/src/incremental.rs:508`, `crates/jurisearch-package-build/src/incremental.rs:526`). Only after that does `producer_cycle` call `publish_package` (`crates/jurisearch-package-build/src/cycle.rs:77`).

If `publish_package` fails after the catalog row is committed but before `root/core/packages/<package_id>` exists, the next run reads that row as the latest package / retained incremental. `build_incremental` then uses the unpublished row as the chain head, and `build_remote_manifest` attempts to read the missing published artifact for every incremental catalog row (`crates/jurisearch-package-build/src/remote_manifest.rs:121`, `crates/jurisearch-package-build/src/remote_manifest.rs:133`). That leaves the outbox high-water mark advanced in the catalog without a corresponding served artifact, and retrying `producer_cycle` cannot republish the existing build because the build step now treats the catalog row as already produced. This is exactly the pre-publish failure window the acceptance gate calls out.

Actionable fix: make catalog advancement atomic with publish visibility, or add an explicit recovery path. For example, keep the build result staged without becoming the latest catalog row until `publish_package` succeeds, then insert/mark the catalog row as published in the same serialized publish phase. If the current built-before-publish catalog row is kept, `latest_package_for_corpus` / manifest selection must ignore non-published rows, and `producer_cycle` must be able to resume a `built` row by publishing the staged artifact and only then marking it published. Add a gated test that injects a publish failure after `insert_package_catalog_row`, reruns the cycle, and asserts the same package is published and the manifest advances only after the artifact exists.

### WARN: `provision-db` ignores the configured writer password file

`ProducerConfig` accepts `writer_password_file` and `writer_handle()` loads it for normal producer DB mutations (`crates/jurisearch-producer/src/config.rs:67`, `crates/jurisearch-producer/src/config.rs:357`, `crates/jurisearch-producer/src/config.rs:363`), but `provision_config()` passes `writer_password: None` and `read_password: None` into `RoleSpec` (`crates/jurisearch-producer/src/config.rs:341`, `crates/jurisearch-producer/src/config.rs:345`, `crates/jurisearch-producer/src/config.rs:349`). The storage provisioner only emits `ALTER ROLE ... PASSWORD` when those fields are populated (`crates/jurisearch-storage/src/backend.rs:700`, `crates/jurisearch-storage/src/backend.rs:707`), and its postcondition probe also uses those same `RoleSpec` passwords when connecting as writer/read (`crates/jurisearch-storage/src/provision.rs:107`, `crates/jurisearch-storage/src/provision.rs:217`).

On a real external PostgreSQL requiring password auth, the example config's `writer_password_file` will be used later by `update`, but `provision-db` never sets that password on the role and may also fail its own writer probe with no password supplied. That makes the advertised blank-external-DB provisioning path unreliable.

Actionable fix: load `writer_password_file` into `RoleSpec.writer_password` in `provision_config()`. If the read role also needs password auth in the external deployment profile, add a `read_password_file` config field and pass it through as well. Add a non-gated unit test around `ProducerConfig::provision_config()` that creates a 0600 writer password file and asserts `roles.writer_password == Some(...)`.

### WARN: successful update checkpoints do not record the package high-water mark

After `producer_cycle` succeeds, `run_update` creates a `PackageHighWaterMark` with both sequence fields set to `None` regardless of whether an incremental was built (`crates/jurisearch-producer/src/update.rs:177`, `crates/jurisearch-producer/src/update.rs:179`, `crates/jurisearch-producer/src/update.rs:181`). The report returns `built_incremental`, but the checkpoint's package coordinate does not include the package sequence or included `change_seq` high. This weakens the three-cursor audit trail the producer is supposed to persist after a successful run.

Actionable fix: extend `ProducerCycleReport` to carry the incremental `to_sequence` and `included_change_seq_high` from `IncrementalBuildReport`, or read the latest package catalog row after the cycle, then populate `PackageHighWaterMark` with real values. For an empty outbox, record the current published head if available and leave only the included window high unchanged when no new incremental exists. Add an assertion in the gated publish test that a real `run_update` checkpoint contains the expected package sequence and `change_seq` high after a one-delta cycle.

## Verified Non-Findings

- The new `jurisearch-producer` CLI/library path is in-process; I did not find `std::process::Command` or shell-out usage in the producer crate.
- The producer mutation path constructs a `WriterHandle` / `ConnectionConfig` via `ProducerConfig::writer_handle()`; `ManagedPostgres` appears only in gated tests as the external test server.
- The `update-core` file lock is acquired before ingest and held through enrich, embed, and `producer_cycle`.
- Fetch remains outside the DB mutation lock, and archive ingest selection uses local DILA archive timestamp/journal state rather than package `change_seq`.
- The producer embedding config keeps `request_model` and external `base_url` separate from `storage_embedding_fingerprint()`, and the example producer/site fingerprints match.

VERDICT: FIXES_REQUIRED
