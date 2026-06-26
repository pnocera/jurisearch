# Advice: remaining package-distribution design questions

This advice assumes the decisions in `work/08-jurisearch-server/2026-06-25-central-ingest-delta-sync-analysis.md` are fixed: per-corpus ordered incremental packages, physical-media full baselines/re-baselines, server-managed read-only data in a separate namespace, a writable app namespace preserved across re-baselines, client-side index builds by default, and warn-and-reject on unmet package conditions.

The important current-code constraints are:

- Today's migrations create unqualified tables, so the server-schema split is a target change, not something the current migration runner already enforces (`crates/jurisearch-storage/src/migrations.rs:23-83`).
- The binary already has a schema-ahead rejection shape: `CURRENT_SCHEMA_VERSION` is 17 and `run_migrations` rejects a database whose `schema_migrations` max is above the binary (`crates/jurisearch-storage/src/migrations.rs:3`, `crates/jurisearch-storage/src/migrations.rs:704-754`).
- LEGI projection is upsert-oriented, not append-only: document rows update validity, payload hash, JSON, hierarchy, and `updated_at`; chunks and graph edges also update on conflict (`crates/jurisearch-storage/src/projection/legi.rs:45-105`).
- Deterministic IDs are already the right package primitive: LEGI version rows validate as `legi:<source_uid>@<valid_from>` (`crates/jurisearch-ingest/src/legi/canonical.rs:52-56`), while jurisprudence decisions validate as `<source>:<source_uid>` (`crates/jurisearch-ingest/src/juri/types.rs:180-184`).
- Derived retrieval sets are intentionally rebuildable: `replace_zone_units_for_document` deletes all units for one decision and reinserts the current set, cascading embeddings (`crates/jurisearch-storage/src/zone_units.rs:120-169`); decision-zone refresh can delete zone units when a row stops being derivable (`crates/jurisearch-storage/src/decision_zones.rs:195-207`); hierarchy backfill can delete `chunk_embeddings` for a document (`crates/jurisearch-storage/src/projection/hierarchy_backfill.rs:216-229`).
- IVFFlat indexes are finalize-time products, not package-portable logical artifacts: dense finalize verifies full embedding coverage, drops/recreates the IVFFlat index, and writes `index_manifest` (`crates/jurisearch-storage/src/dense.rs:93-190`); zone dense finalize does the same for zone-unit embeddings (`crates/jurisearch-storage/src/zone_units.rs:431-524`). BM25 indexes are `pg_search` indexes defined in migrations (`crates/jurisearch-storage/src/migrations.rs:103-105`, `crates/jurisearch-storage/src/migrations.rs:355-369`, `crates/jurisearch-storage/src/migrations.rs:559-573`).
- The current `serve` daemon is single-client/sequential and reuses the same query dispatcher (`crates/jurisearch-cli/src/serve.rs:1-4`, `crates/jurisearch-cli/src/serve.rs:72-123`). A future package service should use that service boundary, but not rely on users closing every CLI session voluntarily.

## 1. Scoped server-schema reload on media re-baseline

Recommendation: use a generationed server schema plus stable views, not in-place `DROP SCHEMA ... CASCADE` as the normal path.

Concretely:

1. Keep the client-visible server namespace stable, for example `jurisearch_server`, but make it contain only views/functions/synonyms over a physical generation schema such as `jurisearch_server_g000123`.
2. Load the media baseline into a new physical schema, for example `jurisearch_server_g000124`, while queries continue reading `jurisearch_server` views that point at `g000123`.
3. Build required client-side indexes inside `g000124` before exposure. For the default path, that means table load, constraints, BM25 indexes, IVFFlat finalization, `ANALYZE`, manifest rows, and validation before switch. This matches the existing finalize shape, where dense indexes are built only after coverage is complete (`dense.rs:122-160`, `zone_units.rs:461-499`).
4. In one short transaction, take a package-apply advisory lock, verify the current corpus cursor still equals the expected old baseline/generation, replace the stable views to point at `g000124`, update the local package cursor/active generation row, and commit.
5. Keep the previous generation for rollback/diagnostics until the new generation has passed a post-switch smoke check and no old transactions are using it; then drop it asynchronously.

The client-visible switch should be a view switch, not `ALTER DATABASE SET search_path`, because `search_path` is connection state and a running CLI/service can keep old session settings. A view switch is visible to new statements after commit and keeps app SQL stable. If performance makes layered views unacceptable for hot paths, use stable SQL functions or a small set of unqualified compatibility views only for public query entry points, but keep the same generation-indirection concept.

Avoid `ALTER SCHEMA old RENAME TO ...; ALTER SCHEMA new RENAME TO jurisearch_server` as the main mechanism if queries use qualified table names: renaming a schema takes stronger catalog locks and causes more surprising plan/cache invalidation. It is workable for maintenance windows, but views give a smaller critical section.

`DROP SCHEMA jurisearch_server CASCADE` + reload should be a disaster-recovery fallback, not the normal operated path. It creates the exact failure mode the design is trying to avoid: any concurrent read sees relation disappearance or blocks behind DDL, and hard writable FKs into the server schema will either block the drop or be dropped with it. It also widens the outage to index build time, which is the expensive part for BM25/IVFFlat.

Locking shape:

- Long phase: load and index `g000124` without touching the active generation. This should not block normal reads except for shared extension/catalog work.
- Short switch: one transaction with a DB-level advisory lock for package apply, a per-corpus cursor check, `CREATE OR REPLACE VIEW`/function replacement, cursor update, and commit. Set a low `lock_timeout` and fail cleanly rather than waiting behind a long user query.
- Cleanup: drop old generation later with a bounded lock timeout. If it cannot drop, mark it `retired` and retry.

Tradeoff: the generation-view design costs extra plumbing and can complicate query planning if every table is behind a view. The payoff is that it keeps the write-heavy/index-heavy re-baseline work off the live read path and avoids making the writable schema participate in destructive DDL.

## 2. Writable-to-server reference strategy

Recommendation: do not use hard cross-schema FKs from the writable app schema into server-managed tables. Use validated soft references, with optional generation-aware validation metadata.

The app schema should store references as ordinary columns plus validation state, for example:

- `target_kind`: `document_version`, `logical_article`, `decision`, `chunk`, `zone_unit`, etc.
- `corpus`.
- `document_id` when pinning a specific immutable version row.
- `source`, `source_uid`, `version_group`, and `as_of_date` when referring to a logical legal object over time.
- `resolved_document_id`, `resolved_generation`, `resolved_schema_version`, `validated_at`, and `validation_status`.

Validation should be done by the local service after package apply and on demand before workflows that need a live target. Re-baseline then becomes: switch server generation, run reference validation in the background, mark missing/changed targets explicitly, and let app UX decide whether to pin, retarget, or warn.

Hard FKs are attractive for simple data integrity, but they are the wrong default here. A re-baseline intentionally drops/replaces the whole server-managed set. Cross-schema FKs would force constraint drop/recreate or `NOT VALID` gymnastics during every media baseline, and they would turn a server reload into a writable-schema migration. That is too much coupling for an extension point whose future tables are out of scope.

Generation-switch design is still useful, but as the server reload mechanism, not as a reason to make app references hard FKs. If the app needs stronger guarantees for a narrow table later, add a client-owned validation table with `(reference_id, active_generation, resolved_document_id, valid)` rather than a direct FK to server physical tables.

Identity convention:

- Reference `document_id` when the app meaning is "this exact version/text I saw." This is appropriate for citations in a generated memo, audit evidence, quoted passages, a saved search result, or any reproducibility-sensitive artifact. Supersession keeps old version rows, and LEGI `document_id` is version-specific by construction (`legi:<source_uid>@<valid_from>`).
- Reference `source_uid`/`version_group` plus `as_of_date` when the app meaning is "the article/legal object applicable at date D" or "track this logical article over time." The resolver should select the server row whose validity contains the as-of date, using `valid_from`/`valid_to`. Store the last `resolved_document_id` as a cache/evidence field, but do not make it the semantic identity.
- For jurisprudence decisions, `document_id` is usually both specific and logical enough because the row ID is `<source>:<source_uid>` and decisions are not temporal versions in the same way. Still store source/source_uid for display and repair.
- For chunks and zone units, prefer anchoring app objects at the document/article level plus offsets/quote hashes, not hard references to chunk IDs or `zone_unit_id`, unless the feature is explicitly retrieval-debugging. Chunking and zone builders can be reissued; the schema already carries `chunk_builder_version` and `zone_unit_builder_version`, which is a warning that these are derived identities.

Tradeoff: soft references require explicit validation jobs and app states for "target missing" or "resolved to a newer version." That is more application code, but it keeps media re-baseline and writable data preservation feasible.

## 3. Incremental diff generation without uniform `updated_at`

Recommendation: build a dedicated package-change ledger/outbox in the server ingest pipeline, written transactionally by projection paths. Use snapshot/hash comparison as an audit/backstop, not the primary diff mechanism. Do not use logical decoding as the first design.

The reason is practical: the code already has central projection boundaries where the semantic changes are known. LEGI writes documents, chunks, and graph edges through prepared upsert statements (`projection/legi.rs:45-105`). Embedding insertions go through batch staging and upsert functions (`projection/embeddings.rs:53-130`; `zone_units.rs:342-418`). Zone units are already per-document replace operations (`zone_units.rs:120-169`). Those are the correct places to emit package events, because they know whether an operation is a base-row upsert, an embedding upsert, or a replace-set.

Suggested ledger shape:

```sql
CREATE TABLE package_change_log (
  corpus text NOT NULL,
  ingest_run_id text NOT NULL,
  change_seq bigserial PRIMARY KEY,
  table_name text NOT NULL,
  op text NOT NULL CHECK (op IN ('upsert', 'delete', 'replace_set')),
  scope_kind text NOT NULL,
  scope_key text NOT NULL,
  row_pk jsonb NOT NULL DEFAULT '{}'::jsonb,
  row_hash text,
  before_hash text,
  after_hash text,
  payload jsonb,
  builder_versions jsonb NOT NULL DEFAULT '{}'::jsonb,
  embedding_fingerprint text,
  schema_version integer NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);
```

For large payloads, the ledger does not need to duplicate every row body. It can record the changed PK/scope and hashes, and the package builder can materialize row payloads from the authoritative server tables at package-build time. What matters is that the ledger is the authoritative list of scopes touched since package N.

Why not generic `updated_at`:

- Only `documents` has `updated_at` in the base schema (`migrations.rs:27-44`).
- `chunks`, `chunk_embeddings`, `zone_units`, and `zone_unit_embeddings` have `created_at` but no update watermark (`migrations.rs:46-67`, `migrations.rs:495-539`).
- Projection updates chunks and graph edges on conflict without stamping an update time (`projection/legi.rs:73-105`).

Why not snapshot/hash as primary:

- It is robust but expensive at corpus scale, especially for vector-heavy tables.
- It struggles to recover semantic replace scopes. It can tell that `zone_unit_embeddings` row X disappeared, but it does not naturally say "replace the complete zone-unit set for document D under builder version V." The client package format needs that semantic operation for idempotence.

Why not logical decoding first:

- It captures low-level row changes, not the package semantics. You would still need to reconstruct per-document replace sets, builder-version boundaries, corpus scoping, and "this is a full reissue" decisions.
- It couples the package producer to server Postgres WAL/slot operations, while the chosen architecture explicitly moved away from native replication machinery.

Use snapshot/hash comparison for package QA: after building package N, compute per-table row counts and ordered hash digests for the affected corpus/generation; include those in the manifest; and periodically compare a package-applied staging database against a direct server snapshot.

Tradeoff: an outbox adds implementation work and requires every mutating projection path to participate. The benefit is that it produces the exact semantic diff the client needs and avoids pretending every table has a clean watermark.

## 4. Derived-table delete/replace in a diff

Recommendation: express derived rebuilds as scoped `replace_set` operations, not per-row delete streams and not generation-wide truncation for ordinary incrementals.

For `zone_units` and `zone_unit_embeddings`, the natural scope is one document:

```json
{
  "op": "replace_set",
  "table_group": "zone_units",
  "scope": { "document_id": "cass:..." },
  "builder_version": "zone-unit-builder-vN",
  "source_text_hash": "...",
  "embedding_fingerprint": "bge-m3:1024:...",
  "rows": {
    "zone_units": [...],
    "zone_unit_embeddings": [...]
  }
}
```

Client apply should run inside the package transaction:

1. Verify package sequence/corpus/fingerprint/builder constraints.
2. For each scoped replacement, delete `zone_units` for that `document_id`; cascade removes old `zone_unit_embeddings`.
3. Insert the provided `zone_units`.
4. Insert matching `zone_unit_embeddings` when included.
5. Verify the set: every provided embedding has a zone unit; every zone unit in the new set has the expected builder version and, if the package requires dense readiness, an embedding with the expected fingerprint.

This mirrors the live derivation writer: `replace_zone_units_for_document` deletes all units for one document then inserts the current rows, and the FK from `zone_unit_embeddings` to `zone_units` is `ON DELETE CASCADE` (`zone_units.rs:120-169`, `migrations.rs:532-539`). It is naturally idempotent: replaying the same operation yields the same set.

For `chunk_embeddings`, use a document-scoped replacement when the chunk set or hierarchy/context changed for a document. The hierarchy backfill already invalidates all chunk embeddings for one document (`hierarchy_backfill.rs:216-229`), and chunk embeddings FK to `chunks(chunk_id) ON DELETE CASCADE` (`migrations.rs:60-67`). If only a single embedding payload is corrected without chunk-set churn, an `upsert` by `chunk_id` is fine; the package builder can emit the smaller op. But any operation that changes chunk membership, contextualized body, or fingerprint should be document-scoped so stale rows cannot survive.

Do not emit per-row deletes as the primary representation. They are verbose, easier to get wrong, and do not encode the invariant that the whole derived set for a scope is being replaced. They are acceptable for rare base redactions or one-off repair events, but not for routine derived rebuilds.

Do not use generation-wide truncation for ordinary diffs. It is appropriate only for a full baseline/re-baseline or a builder/fingerprint full reissue. In ordinary packages, truncation destroys locality and forces the client into a large unavailable window.

Keys/version stamps needed:

- Scope key: `document_id` for document-derived sets; possibly `(corpus, document_id)` if a DB hosts multiple corpora in one server namespace.
- Stable row PKs: `zone_unit_id`, `chunk_id`.
- Builder stamps: `chunk_builder_version`, `zone_unit_builder_version`, `zone_schema_version`.
- Embedding stamps: `embedding_fingerprint`, `model`, `dimension`, `normalize`.
- Source/provenance stamps: `source_payload_hash` for documents/chunks, `text_hash` for zone units, and package `from_sequence`/`to_sequence`.
- Optional set digest: a deterministic hash over the ordered PK+row_hash list for the replacement scope. The client can compute this after apply and fail the package before cursor advance if it does not match.

Ordered apply makes these operations safe: a client must be at `from_sequence`; it applies the replace set; it verifies per-scope digest; only then does it advance to `to_sequence`. A retry sees the same `from_sequence` unless the previous transaction committed; if it committed, the cursor already advanced and the package is skipped as already applied.

## 5. Catch-up for a long-offline client

Recommendation: support both paths, but make the server manifest tell the client when incremental catch-up is still the preferred route. Use cumulative compressed payload plus estimated apply/index cost, not chain length alone.

A practical policy:

- Retain incremental packages for at least a fixed window, e.g. 90 days, and a fixed count, e.g. the latest 120 daily packages per corpus. A client behind the retained `min_available_sequence` must use a fresh baseline.
- Prefer incremental catch-up while all are true:
  - no gap between client sequence and manifest head;
  - every package is compatible with the client version, schema version, embedding fingerprint, and builder versions;
  - cumulative compressed diff size is less than 25-35% of the compressed current baseline size;
  - estimated apply work is less than a threshold, for example 30-45 minutes on the reference client profile;
  - no package in the range is marked `requires_baseline_after_apply` or `superseded_by_baseline`.
- Prefer fresh baseline when any are true:
  - client sequence is below `min_available_sequence`;
  - cumulative compressed diffs exceed about one third of baseline size;
  - cumulative uncompressed row/vector bytes exceed about 50% of baseline bytes;
  - the range crosses an embedding fingerprint or builder-version full reissue;
  - the package chain includes a breaking schema/corpus rewrite;
  - expected apply/index time is worse than media baseline load time.

The exact thresholds should be manifest-configured per corpus. Vectors make byte size a better proxy than package count; 10 small legal-text corrections are not the same as 10 packages containing millions of refreshed embeddings.

Concrete manifest fields to support this:

- Current baseline sequence and baseline artifact size.
- `min_available_sequence`.
- Per-package compressed/uncompressed sizes and estimated apply cost.
- A precomputed `catchup_ranges` section such as: from sequence 1000 to head 1088 is incremental-ok; from sequence 800 requires baseline `core-baseline-2026-06`.

Tradeoff: retaining many small increments improves offline tolerance but increases hosting, signing, and QA surface. Re-baseline media is operationally heavier for users but gives a cleaner and faster recovery once drift is large.

## 6. Package manifest contract

Recommendation: separate the per-corpus remote manifest from each package's embedded manifest, but make both signed and make the package manifest self-sufficient. The client should never need to trust only the remote listing after it has downloaded an artifact.

Per-corpus remote manifest fields:

```json
{
  "manifest_version": 1,
  "generated_at": "2026-06-26T00:00:00Z",
  "publisher": "jurisearch",
  "corpus": "core",
  "environment": "production",
  "head_sequence": 1088,
  "min_available_sequence": 970,
  "active_baseline": {
    "baseline_id": "core-2026-06-25-g000124",
    "generation": "g000124",
    "sequence": 1040,
    "schema_version": 17,
    "artifact_uri": "...",
    "compressed_size_bytes": 123,
    "sha256": "...",
    "signature": "..."
  },
  "packages": [
    {
      "package_id": "core-1041-1042",
      "from_sequence": 1041,
      "to_sequence": 1042,
      "artifact_uri": "...",
      "compressed_size_bytes": 123,
      "uncompressed_size_bytes": 456,
      "row_counts": { "documents": 10 },
      "requires_baseline": false,
      "minimum_client_version": "x.y.z",
      "schema_version": 17,
      "embedding_fingerprint": "bge-m3:1024:cls:normalize=true",
      "builder_versions": {
        "chunk_builder_version": "...",
        "zone_unit_builder_version": "..."
      },
      "sha256": "...",
      "signature": "..."
    }
  ],
  "catchup_policy": {
    "max_incremental_packages": 120,
    "max_cumulative_diff_to_baseline_ratio": 0.33
  },
  "entitlement": {
    "corpus": "core",
    "tier": "open-or-subscription",
    "license_epoch": 3,
    "audience": "..."
  },
  "signing": {
    "key_id": "...",
    "algorithm": "..."
  }
}
```

Package embedded manifest fields:

- Identity and ordering:
  - `package_format_version`.
  - `package_id`.
  - `corpus`.
  - `package_kind`: `incremental`, `baseline`, or `rebaseline`.
  - `from_sequence`, `to_sequence`, and `previous_package_id`.
  - `previous_package_sha256` or previous manifest digest.
  - `baseline_id` and `generation`.
  - `created_at`, `builder_host_id` or builder run ID.

- Compatibility gates:
  - `minimum_client_version`.
  - `maximum_client_version` only if a known-bad newer range ever exists; otherwise omit.
  - `schema_version` and schema migration bundle digest.
  - `requires_extensions`: `vector`, `pg_search`, required versions if known.
  - `postgres_major_min`/`postgres_major_max` only if using any physical-format variant. For the default logical package path, this should normally be absent or advisory.
  - `embedding_fingerprint`, model, dimension, normalize.
  - `builder_versions`: chunk builder, zone-unit builder, zone schema, citation extractor/resolver if those outputs are packaged.

- Entitlement:
  - `entitlement_corpus`.
  - `tier`/SKU.
  - `license_epoch`.
  - optional `audience` or tenant/customer scope if packages are personalized.
  - an entitlement policy digest so the client can explain "not subscribed to corpus X" rather than a generic integrity failure.

- Integrity and signing:
  - artifact `sha256`.
  - uncompressed payload digest.
  - per-file digests for each table/change file.
  - manifest canonicalization algorithm.
  - signature algorithm, signature, key ID, certificate/key epoch.
  - optional transparency/log index if you add supply-chain audit later.

- Apply contract:
  - `expected_client_from_sequence`.
  - `result_sequence`.
  - `requires_empty_generation` for baseline/rebaseline.
  - `schema_ops_digest`.
  - `operations` summary: counts by table and operation kind (`upsert`, `delete`, `replace_set`).
  - `replace_scopes` counts and optional scope digests.
  - `preconditions`: current schema version, current embedding fingerprint, current builder versions, active baseline/generation.
  - `postconditions`: expected row counts and deterministic table/set digests after apply.
  - `index_build`: BM25 indexes to build, IVFFlat indexes to finalize, lists/probes defaults, and whether package is queryable before finalize. The default should be "not advertised as active until indexes are built and manifests written."
  - `idempotency_key`: usually `package_id` + digest.
  - `rollback_policy`: transaction rollback for incremental packages; for baseline generation switch, keep previous generation until post-apply validation succeeds.

- Payload layout:
  - file list with table name, operation kind, format (`copy-binary`, `jsonl`, `parquet`, etc.), compression, row count, digest.
  - dependency order for apply: base tables before dependent tables; derived replace scopes after base; embeddings after chunks/zone units; index finalize last.

- Warn-and-reject reasons:
  - explicit machine-readable error codes the client can surface: `client_too_old`, `schema_ahead`, `missing_entitlement`, `sequence_gap`, `wrong_generation`, `embedding_fingerprint_mismatch`, `builder_version_mismatch`, `signature_invalid`, `digest_mismatch`, `extension_missing`, `baseline_required`.

Ordered apply enforcement:

The local client should keep a server-managed control table outside the swappable generation, for example `jurisearch_control.corpus_state`, with `corpus`, `active_generation`, `sequence`, `baseline_id`, `schema_version`, `embedding_fingerprint`, builder versions, last applied package ID/digest, and applied-at timestamp. The apply transaction must check `sequence == package.from_sequence - 1` (or whatever exact convention is chosen) and update it to `to_sequence` only after all data, indexes, and postcondition checks pass. This prevents gaps and makes retries deterministic.

Integrity enforcement:

Verify manifest signature before download decisions; verify artifact digest after download; verify embedded manifest signature/digest before unpacking; verify per-file digests before applying; verify post-apply row/set digests before cursor advance. Any failure is warn-and-reject, with no partial cursor movement.

Entitlement enforcement:

The remote manifest should be filtered by subscription when possible, but the client should also verify that the package's embedded entitlement corpus/tier matches a locally installed license token. Entitlement is not just hiding URLs; it is an apply precondition.

Tradeoff: this manifest is more verbose than a simple package list. That is intentional. The package model replaces native replication guarantees, so the manifest has to carry ordering, compatibility, entitlement, integrity, and explainable rejection semantics explicitly.

## Bottom-line recommendations

1. Use staged generation schemas plus a short stable-view switch for media re-baselines. Do not normally drop the active server schema in place.
2. Use soft, validated writable-to-server references. Pin exact evidence by `document_id`; track logical LEGI articles by `source_uid`/`version_group` plus `as_of_date`.
3. Add an ingest-side semantic change log/outbox at projection boundaries. Use snapshot/hash comparison for audit, not primary diff generation.
4. Model derived rebuilds as scoped `replace_set` operations, usually per document, with builder/fingerprint stamps and set digests.
5. Catch up incrementally while the chain is retained and the cumulative cost is small; require a fresh baseline when the chain is missing, crosses a full reissue, or exceeds roughly one third of baseline size or the configured apply-time budget.
6. Make manifests explicit and signed: sequence chain, baseline/generation, compatibility gates, entitlement, per-file integrity, apply preconditions/postconditions, index-build contract, and machine-readable warn-and-reject reasons.
