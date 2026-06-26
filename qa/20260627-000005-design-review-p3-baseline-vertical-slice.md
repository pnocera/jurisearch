# P3 Baseline Vertical Slice Architecture Review

Overall recommendation: **GO with targeted adjustments before implementation**. The proposed split and long-phase/switch-phase architecture match the committed P2 topology. The changes I would make now are mostly about avoiding accidental coupling to `public`, making baseline load constraints/indexes explicit, and preventing a loopback-only binary payload from becoming the implied portable package contract.

The current source supports the plan:

- `jurisearch-package` already has the right contract shape: `EmbeddedManifest` carries compatibility, integrity, payload layout, apply contract, postconditions, index-build contract, and an idempotency key. `PayloadFormat::CopyBinary` exists, and `StubSigner` / `AcceptAllVerifier` are explicitly loopback/test signing surfaces.
- `generations.rs` already implements the P2 switch correctly: build a generation in `building`, atomically activate under advisory lock with cursor guard, repoint `jurisearch_server` views, and update `jurisearch_control.corpus_state` in one transaction.
- `runtime.rs::execute_read_sql` is already generation-aware: no active corpus reads `public`; one active corpus uses the active physical generation schema first; multiple active corpora use `jurisearch_server, public`.
- `dense.rs` and `zone_units.rs` finalizers are public/re-embed-oriented, not generation baseline builders.
- `outbox::corpus_table_digests` is the right QA idea but is currently hardwired to unqualified producer tables and needs to be generalized before using it as an apply postcondition.

## D1 - Crate Layout

**GO.** Add `jurisearch-package-build` and `jurisearch-syncd`.

Keep the split you proposed:

- `jurisearch-package-build`: producer baseline builder and catalog writer, depending on `jurisearch-package` and `jurisearch-storage`.
- `jurisearch-syncd`: consumer binary for `verify -> apply -> status`, depending on `jurisearch-package` and `jurisearch-storage`.
- `jurisearch-storage`: transactional apply primitives, generation DDL, validation helpers, index-build helpers.
- `jurisearch-cli`: optional thin command wrapper that calls the builder, not the owner of package-build logic.

That is the right boundary because `serve.rs` is a query daemon with a JSONL session protocol and should not become the package apply/build orchestration surface. Starting the builder inside `jurisearch-cli` would look cheaper for P3, but it would put P4/P5 producer catalog and package-chain behavior in the wrong crate immediately.

Implementation note: add both crates to workspace members and keep P3 APIs narrow. Do not build a generic package framework yet; implement baseline build/apply with manifest validation and leave incremental package generalization for P4.

## D2 - Payload Format

**ADJUST.** Rows as payloads are correct. Defaulting P3 to `COPY BINARY` is acceptable only if it is explicitly treated as a same-Postgres loopback encoding, not as the durable baseline package default.

For P3, `COPY ... FORMAT binary` is pragmatic because the acceptance path is same PG major and same architecture. It will be fast, simple, and already maps to `PayloadFormat::CopyBinary`. But binary COPY is coupled to PostgreSQL type layout, exact column order, and server-version assumptions. That is fine for a vertical slice; it is not the portable package contract implied by physical media baselines in the long-term distribution model.

Recommended P3 contract:

- Use one payload file per replicated table.
- Use explicit column lists for both export and import. Do not rely on `SELECT *` or physical table order.
- Exclude generated columns from COPY input, matching the existing `populate_generation_from_public` discipline.
- Bind each file to schema identity with table name, column list, payload format, row count, digest, and schema/schema-op digest in the manifest.
- When `CopyBinary` is used, require compatibility fields to constrain it: PostgreSQL major min/max should match the producer for P3, and syncd should reject mismatches.

If you want the first implementation to be contract-correct beyond loopback, add a `CopyText` or JSONL path now and make `CopyBinary` an optimization. If the goal is the P3 loopback slice, ship `CopyBinary`, but label it as loopback-only in the manifest/apply guard.

## D3 - Index Build on the Generation Clone

**ADJUST, but the direction is right.** Add a load-oriented generation creation path. Do not bulk-load into fully indexed tables.

`create_generation_schema` currently uses:

```sql
CREATE TABLE <generation>.<table> (LIKE public.<table> INCLUDING ALL)
```

That is fine for P2 topology tests, but it is the wrong default for baseline load because it clones index definitions before the data is loaded. For a baseline you want:

1. Create generation schema.
2. Create tables with columns/defaults/generated definitions/check constraints needed for valid rows.
3. Bulk-load rows.
4. Add primary/unique/FK constraints and indexes.
5. Build BM25/IVFFlat with corpus-sized parameters.
6. Analyze and validate.
7. Activate.

So yes: add `create_generation_schema_for_load` or an equivalent load mode rather than changing the existing helper.

Important caveat: do not assume `LIKE ... INCLUDING ALL EXCLUDING INDEXES` preserves all the constraints you care about. PostgreSQL primary keys and unique constraints are backed by indexes; excluding indexes means you should explicitly verify what survives and, for P3, preferably create the post-load constraint/index set intentionally. This is the biggest D3 footgun. If you implement load schema creation as "LIKE excluding indexes" and never recreate PK/unique/FK constraints, the baseline may validate by digest while producing a weaker physical schema than the producer.

Concrete recommendation:

- Use a table-clone helper for column shape.
- Use a separate post-load helper for constraints/indexes.
- Keep index names schema-qualified and generation-specific.
- Add one test that proves a loaded generation has the expected PK/unique/FK/index inventory before activation.

## D4 - Index-Build Primitives

**GO.** Add generation-schema-aware index build helpers in storage. Do not reuse the re-embed finalizers directly.

`dense.rs::finalize_dense_rebuild` and `zone_units.rs::finalize_zone_dense_rebuild` are the wrong abstraction for baseline apply. They target unqualified/public tables, stamp embedding fingerprints, emit outbox rows, write global `index_manifest`, and assume embeddings were just rebuilt. For P3 baseline apply, the embeddings are already in the payload.

Add storage helpers with a shape closer to:

```rust
build_generation_indexes(client, schema, corpus, index_spec)
```

They should:

- Validate loaded embedding coverage if cheap enough for P3.
- Compute IVFFlat `lists` with `recommended_ivfflat_lists(row_count)`.
- Create/recreate IVFFlat indexes on `schema.chunk_embeddings` and `schema.zone_unit_embeddings`.
- Create BM25/search indexes with schema-qualified DDL.
- Run `ANALYZE` on the generation tables used by retrieval.
- Return index metadata for the activation/cursor record.

One adjustment: be careful with `index_manifest`. P2 currently lists `index_manifest` as non-generation/global, while the P3 plan says "write `index_manifest` rows" before activation. That is safe only for a single-corpus loopback if no reader treats global `index_manifest` as readiness for a building generation. Longer term, index metadata needs to be generation-scoped, either in `jurisearch_control.generation_registry`/a side table or in per-generation metadata surfaced through the active view. I would not let P3 depend on a global `index_manifest` row as the proof that a not-yet-active generation is ready.

## D5 - Producer Catalog

**GO with small additions.** Add migration v21 `package_catalog` in `jurisearch-storage`; storage is the right owner.

The proposed columns are directionally right:

- `corpus`
- `package_sequence`
- `baseline_id`
- `package_kind`
- `package_id`
- `included_change_seq_high`
- `previous_package_id`
- `status`
- `created_at`

I would add or reserve these now because they prevent P4 churn:

- `package_digest`
- `manifest_digest`
- `previous_package_digest`
- `schema_version`
- `embedding_fingerprint`
- `builder_versions`
- `published_at`
- `updated_at`

Constraints:

- Primary key on `(corpus, package_sequence)`.
- Unique `package_id`.
- Status check/enum for at least `built`, `published`, `failed`.
- `included_change_seq_high` `NOT NULL`, even for baseline, so the baseline has a clear outbox high-water mark for the first incremental.

Keep global `change_seq` distinct from per-corpus `package_sequence`. The catalog row should freeze the high-water mark used to build the package, not imply that `change_seq` and package sequence are the same axis.

## D6 - Postcondition Validation

**ADJUST.** Use `corpus_table_digests` semantics, but refactor it before P3 apply validation.

The existing `outbox::corpus_table_digests(postgres, corpus)` is the right primitive conceptually: table-by-table row counts and deterministic JSON digests with volatile columns removed. But the current implementation opens a normal storage connection and queries unqualified relation names, so it targets the producer/read path. It cannot directly validate a not-yet-active generation.

Cleanest implementation:

- Extract the digest table specs into a reusable internal helper.
- Add a source parameter, for example:

```rust
enum DigestSource<'a> {
    ProducerPublic,
    Generation { schema: &'a str },
}
```

- Generate schema-qualified SQL for generation validation.
- Keep the same corpus predicates where possible, even inside a single-corpus generation, so a wrong-corpus row is caught.
- Preserve the volatile-column exclusions exactly in both producer and generation modes.

Then use the same code path for:

- Producer manifest postconditions.
- Consumer pre-switch generation validation.

That avoids the classic failure mode where producer and consumer "digests" are two similar but not identical implementations.

## D7 - Apply Ordering and Atomicity

**GO with one idempotency adjustment.** The long-phase/building-generation model and atomic `activate_generation` call are the right shape.

Use `PayloadLayout.apply_order` and keep `citation_order_holds()` as a package validation gate. Load into an invisible `building` generation, build indexes, analyze, validate postconditions, then call `activate_generation`. That satisfies the P2 switch model: the only globally visible mutation is the activation transaction that updates generation state, repoints views, and writes `corpus_state`.

For idempotency, `corpus_state.sequence >= result_sequence` is necessary but not sufficient. A no-op should require identity compatibility too:

- If `corpus_state.sequence == result_sequence` and `last_package_id` / `last_package_digest` match, no-op.
- If `corpus_state.sequence > result_sequence`, report already ahead and reject or skip explicitly; do not silently treat it as the same package.
- If sequence matches but digest/package id differs, reject loudly.

Also do the idempotency check before creating/loading a new generation. If a previous attempt failed after creating a building generation but before activation, the cursor will not have advanced; retry should either create a fresh generation number or cleanly mark/drop the failed building generation, but it must not infer success from the presence of a generation schema.

## INV-6 Query Readiness

Your main INV-6 reasoning is correct: a building generation is invisible because `corpus_state` points only at the active generation, and `execute_read_sql` derives the read path from active cursor state.

The gaps to close are metadata/cache related:

- Do not publish readiness through global `index_manifest` for a building generation.
- Ensure any query-readiness cache includes active generation/sequence in its signature or is invalidated after activation. The runtime code already appears to be active-read-signature-aware; keep that invariant in the syncd activation path.
- Index validation should happen before `activate_generation`, and activation should write enough metadata for later readiness checks to prove that the active generation has the expected BM25/IVFFlat/index fingerprint.

For P3 single corpus, you can get away with minimal generation metadata. Do not hard-code global `index_manifest` as the long-term readiness source.

## Schema Migration Bundle

**ADJUST.** P3 can defer applying migration bundles, but it should not omit the manifest contract.

The package manifest already has `schema_migration_bundle_digest` and `Compatibility.schema_version`. Use them in P3:

- Require the client database to already be at the package `schema_version`.
- Reject if it is not.
- Put a real digest in `schema_migration_bundle_digest`, even if the "bundle" for P3 is an empty/already-installed bundle or the digest of the committed migration set.

Current source has `CURRENT_SCHEMA_VERSION = 20`; adding `package_catalog` as v21 changes the compatibility target. Because `package_catalog` is producer-side but lives in shared storage migrations, be explicit about whether syncd clients must also be migrated to v21 before apply. My recommendation: require syncd's local storage schema to be current before applying P3 packages, and defer shipping/applying the migration bundle itself until a later phase.

## Missing Items That Could Force Rework

1. **Constraint recreation after load.** If `create_generation_schema_for_load` excludes indexes and you do not recreate PK/unique/FK constraints, the generation can become structurally weaker than `public`. Handle this in P3.

2. **Generation-scoped index metadata.** Global `index_manifest` is a poor fit for per-generation readiness. P3 can be minimal, but the activation record or registry should carry enough index fingerprint/status to avoid reworking readiness later.

3. **Digest source parameterization.** Do this before wiring validation. It is small now and expensive to unwind after producer/apply use separate SQL.

4. **Binary COPY guardrails.** If `CopyBinary` ships without strict PG/schema/column-list checks, it will silently become a non-portable contract. Make the loopback limitation explicit in compatibility enforcement.

5. **Package identity in idempotency.** Cursor sequence alone is not a safe no-op key. Include package id/digest comparison.

6. **Workspace integration.** Add the two crates and the v21 migration in one coherent change so `CURRENT_SCHEMA_VERSION`, manifest compatibility, and syncd preflight agree.

Bottom line: **GO** on the P3 architecture if you make the D2/D3/D6/D7 adjustments above. The largest design correction is not the crate split; it is making baseline load a true "load first, then constraints/indexes, then validate, then activate" path with generation-qualified validation and metadata.
