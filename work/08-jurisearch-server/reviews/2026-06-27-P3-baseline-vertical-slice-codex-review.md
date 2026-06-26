# P3 Baseline Vertical Slice Code Review

Review scope: uncommitted and untracked P3 working tree in `/home/pierre/Work/jurisearch`, against the Phase 3 goal/acceptance in `work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-implementation-plan.md`, the package-distribution design/conception docs, and the prior P3 design review adjustments in `qa/20260627-000005-design-review-p3-baseline-vertical-slice.md`.

I did not rerun the already-reported green validation suite. I reviewed the live source, diff, new crates, and the relevant generation/build/apply paths.

## Findings

### BLOCKER 1 - Baseline high-water mark is not tied to the baseline snapshot

`build_baseline` does not cut the baseline from one database snapshot. It computes producer postcondition digests first (`crates/jurisearch-package-build/src/baseline.rs:104`), then opens a separate client and copies each table (`crates/jurisearch-package-build/src/baseline.rs:114`, `crates/jurisearch-package-build/src/baseline.rs:119`), and only after all payload files are written does it read `current_change_seq` for the catalog high-water mark (`crates/jurisearch-package-build/src/baseline.rs:163`). None of those reads share a transaction or repeatable-read snapshot.

That breaks the Phase 3 catalog contract: the catalog row is supposed to record the `change_seq` high-water mark of the baseline snapshot. With concurrent ingest/outbox activity, a mutation can land after a table was copied but before `current_change_seq` is read. The resulting baseline payload will not contain that mutation, but `included_change_seq_high` will include it, so the first incremental will use a `lo` that skips the change. A mutation between the digest pass and the COPY pass can also make the artifact fail its own postconditions.

Actionable fix: build the baseline under one producer snapshot. Open a single `REPEATABLE READ` transaction or exported snapshot and run the digest read, table COPY reads, and high-water mark read through that snapshot. Freeze `included_change_seq_high` from that same snapshot, not from wall-clock state after the payload loop. Plumb the storage helpers so `corpus_table_digests`, COPY-out selection, and `current_change_seq` can operate on the same `GenericClient`/transaction. Add a focused test that inserts a table row plus outbox row while a baseline build is in progress and proves the row is either in both the baseline and catalog window or in neither.

### WARN 1 - The consumer does not enforce the signed schema/extension/index contract

The builder signs/stamps the fields needed for P3 compatibility: `schema_migration_bundle_digest`, `requires_extensions`, PG major bounds, and the index-build contract (`crates/jurisearch-package-build/src/baseline.rs:194`, `crates/jurisearch-package-build/src/baseline.rs:198`, `crates/jurisearch-package-build/src/baseline.rs:199`, `crates/jurisearch-package-build/src/baseline.rs:251`). The applier only checks `schema_version` and the binary-COPY PG major guard before loading (`crates/jurisearch-syncd/src/apply.rs:70`, `crates/jurisearch-syncd/src/apply.rs:135`, `crates/jurisearch-syncd/src/apply.rs:167`). It never compares the manifest's schema bundle digest, never verifies the declared extension set, and never validates the generated indexes against `manifest.apply.index_build`.

`build_generation_indexes` also derives the index inventory from the local client's `public` schema (`crates/jurisearch-storage/src/generations.rs:252`) and recomputes IVFFlat list counts locally (`crates/jurisearch-storage/src/generations.rs:295`), instead of treating the manifest as the apply contract. A client that is nominally at schema version 21 but has drifted extension/index state can therefore either fail after creating a building generation or, worse, activate a generation that does not satisfy the producer-declared index contract.

Actionable fix: add a preflight that recomputes the client's schema bundle digest and checks it against `manifest.compatibility.schema_migration_bundle_digest`; query `pg_extension` for every `requires_extensions` entry; enforce `minimum_client_version` when a client version constant exists. Pass the manifest index contract into the generation index builder or add a post-build validation step that proves every declared BM25 and IVFFlat index exists with the expected method/target/list count before `activate_generation`.

### WARN 2 - Load-mode generations still do not prove the FK inventory

The P3 design review explicitly called out PK/unique/FK/index inventory as the load-mode footgun. The new load path clones tables with `LIKE public.<table> INCLUDING ALL EXCLUDING INDEXES` (`crates/jurisearch-storage/src/generations.rs:168`) and then explicitly recreates only primary/unique constraints (`contype IN ('p','u')`) plus non-IVFFlat indexes (`crates/jurisearch-storage/src/generations.rs:226`, `crates/jurisearch-storage/src/generations.rs:252`). The replicated schema contains important foreign keys, for example `chunks.document_id -> documents.document_id`, `chunk_embeddings.chunk_id -> chunks.chunk_id`, `zone_unit_embeddings.zone_unit_id -> zone_units.zone_unit_id`, and citation rows to `official_api_responses.response_id` (`crates/jurisearch-storage/src/migrations.rs:46`, `crates/jurisearch-storage/src/migrations.rs:60`, `crates/jurisearch-storage/src/migrations.rs:532`, `crates/jurisearch-storage/src/migrations.rs:650`).

The new inventory test checks a documents PK and index access methods, but it would not catch missing generation-local FKs. That leaves the activated generation structurally weaker than the producer schema even when postcondition digests match.

Actionable fix: explicitly recreate and validate `pg_constraint.contype = 'f'` constraints for replicated tables after load, rewriting referenced replicated tables into the same generation schema and documenting any intentional cross-schema exception. Extend the inventory test to compare the expected PK/unique/FK constraint definitions and index definitions between `public` and the generation before activation.

### WARN 3 - Catalog conflict handling can leave stale baseline metadata

P3 always builds the first baseline as package sequence 1 and package id `{corpus}-1-1` (`crates/jurisearch-package-build/src/baseline.rs:97`, `crates/jurisearch-package-build/src/baseline.rs:99`). The catalog insert ignores conflicts on `package_id` (`crates/jurisearch-storage/src/package_catalog.rs:46`, `crates/jurisearch-storage/src/package_catalog.rs:52`). If the same baseline is rebuilt after fixture/data/parameter changes, the artifact and manifest can change while the existing catalog row silently keeps the old digest and `included_change_seq_high`. The same seed also writes `package_digest` and `manifest_digest` to the same manifest digest (`crates/jurisearch-package-build/src/baseline.rs:266`, `crates/jurisearch-package-build/src/baseline.rs:285`), so the catalog does not preserve a separate artifact/package digest for the future chain link.

Actionable fix: make catalog idempotency identity-checked instead of silent. On `package_id` conflict, fetch the existing row and return success only if all immutable fields match, including baseline id, sequence, high-water mark, package digest, manifest digest, schema version, embedding fingerprint, and builder versions; otherwise return a build error. Store a real package/artifact digest separately from the manifest digest.

### NIT 1 - COPY payload file digests are not deterministic across equivalent builds

`baseline_copy_out_select` emits `SELECT ... FROM public.<table> ...` without an `ORDER BY` (`crates/jurisearch-storage/src/generations.rs:399`). PostgreSQL is free to return rows in different physical orders, so the binary COPY payload and per-file digest can change across equivalent builds even when the table content digest is stable.

Actionable fix: add table-specific ordering to the COPY-out SELECT, ideally using the same primary/order key already used by the digest specs. That makes per-file digests and the aggregate payload digest reproducible.

## Confirmed Behavior

The INV-6 shape is otherwise correct in the reviewed source. `apply_baseline` copies payloads, builds generation indexes, and validates postconditions before calling `activate_generation` (`crates/jurisearch-syncd/src/apply.rs:98`, `crates/jurisearch-syncd/src/apply.rs:101`, `crates/jurisearch-syncd/src/apply.rs:103`, `crates/jurisearch-syncd/src/apply.rs:119`). `activate_generation` is the first step that writes `corpus_state` and marks the generation active (`crates/jurisearch-storage/src/generations.rs:691`, `crates/jurisearch-storage/src/generations.rs:705`). Read routing derives active schemas from `corpus_state`, not from `generation_registry`, so a `building` generation is not advertised query-ready (`crates/jurisearch-storage/src/runtime.rs:293`).

The build->apply postcondition digest equality is also wired through one digest implementation: producer manifests call `corpus_table_digests(..., DigestSource::ProducerPublic)`, and syncd validates against `DigestSource::Generation` before activation (`crates/jurisearch-package-build/src/baseline.rs:104`, `crates/jurisearch-syncd/src/apply.rs:327`).

VERDICT: FIXES_REQUIRED
