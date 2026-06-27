# P4 Incremental Vertical Slice Architecture Review

Overall: **GO, but only with several P4-level adjustments before implementation**. The committed P0-P3 code has the right foundations: typed event kinds, `ReplaceSet`, generation-qualified digest validation, a producer catalog, baseline payload guards, active physical generation lookup, and baseline idempotency by package id + package digest. The proposed incremental architecture is directionally correct, but four details would force rework if built as stated:

- `current_change_seq` in a repeatable-read snapshot is **not sufficient by itself** under concurrent outbox writers, because `bigserial` allocation order is not commit order.
- The proposed replace-set mapping omits `decision_zones` and under-specifies document-owned child sets touched by a `documents` scope.
- The current artifact helpers assume one `<table>.copybin` file; incrementals need stable logical file names keyed by table/group + op.
- P4 postcondition validation must run inside the same apply transaction/client, not through the P3 baseline helper that opens a separate connection.

## D1 - Catalog Window

**GO.** Add `package_catalog.included_change_seq_low bigint NOT NULL DEFAULT 0` in v22 and store the full `(low, high]` window.

Storing `low` is better than deriving it forever from the previous row. The plan explicitly names the full window, and a self-contained row makes audits, rebuild detection, and "what exactly did this package claim to cover?" checks much simpler. It also lets you identity-check rebuilds without depending on a join to the previous row.

Concrete changes:

- Add the column with default `0`; the existing baseline rows naturally become `(0, included_change_seq_high]`.
- Extend `PackageCatalogRow` and `insert_package_catalog_row`.
- Include `included_change_seq_low` in the immutable identity comparison.
- For incrementals, read the previous row for the corpus, set `low = previous.included_change_seq_high`, `high = fenced current_change_seq`, and insert the next `package_sequence`.

Also add a per-corpus package-build serialization point. Two builders for the same corpus must not both read the same previous catalog row and race to insert the same next package sequence. Use a producer-side advisory lock keyed by corpus, or `SELECT ... FOR UPDATE` on the latest catalog row inside the catalog-write transaction plus a unique-conflict retry policy. Cross-corpus builds can remain independent.

## D2 - Incremental Builder

**ADJUST.** Materializing current state from changed scopes is the right model, but the scope-to-payload classification needs to be tightened against the actual outbox hooks and `ReplaceSetGroup` enum.

The source confirms the outbox records scopes, not row deltas: `scopes_changed_for_corpus` returns `(change_seq, table_name, op, scope_kind, scope_key)` in global `change_seq` order. So yes, the builder should rematerialize the current authoritative state in a frozen snapshot rather than trying to reconstruct historical row deltas.

However, do not iterate raw `ChangedScope`s one-for-one. The read API returns every ledger row, not a coalesced set. P4 should first collapse the window into one final action per semantic scope, preserving enough "widest required operation wins" information. Examples:

- Multiple `zone_units` replace_set events for the same document in one window should emit one final `ZoneUnits` replace_set.
- `documents` upsert plus `chunk_embeddings` changes for the same document should not produce duplicate/conflicting chunk payloads.
- A wider `ChunksWithEmbeddings` replacement must dominate a narrower `ChunkEmbeddings` replacement for the same document.

Recommended mapping:

- `official_api_responses` / `upsert`: base upsert by `response_id`, carrying the producer `response_id` explicitly.
- `legislation_citation_resolutions` / `upsert`: base upsert by `(corpus, citation_key)`.
- `legi_metadata_roots` / `upsert`: base upsert by root key.
- `documents` / `upsert`: emit the document row and also handle document-owned child state. At minimum, emit `ChunksWithEmbeddings` for the document when the document projection can change chunk membership/body. Otherwise the stale-BM25 acceptance test is not credible.
- `chunks` / `upsert`: current chunk rows for the document are acceptable for dense-finalize parent fingerprint stamping, but if the change represents membership/body/partitioning, widen to `ChunksWithEmbeddings`.
- `chunk_embeddings` / `upsert` or `replace_set`: use `ChunkEmbeddings` only when the chunk row set is known unchanged; otherwise widen to `ChunksWithEmbeddings`.
- `zone_units` / `replace_set`, `zone_units` / `upsert`, and `zone_unit_embeddings` / `upsert`: collapse to `ZoneUnits` for the document. The current enum has no `ZoneUnitEmbeddings`-only group, and a wider set replacement is safe.
- `decision_zones` / `replace_set`: **missing in the proposal**. The storage writer emits this event. Add `ReplaceSetGroup::DecisionZones` or define an explicit singleton-table replace-set handling for `decision_zones`; otherwise P4 cannot apply all emitted event kinds.
- `decision_legislation_citations` / `upsert`: document-scoped current occurrence rows are acceptable for the current additive writer. If citation extraction ever deletes no-longer-present occurrences, this must become a document-scoped replace-set or carry explicit delete keys.
- `graph_edges`: the proposal treats this as a base upsert table, but the current document projection emits only a `documents` scope covering graph edges. If graph edge membership can shrink, simple upserts will leave stale edges. Either add a graph-edge replace-set group/document-scoped delete+insert operation, or explicitly document that P4 does not yet handle graph-edge shrinkage and keep the acceptance surface away from it.

The biggest correctness point is `chunks`: the design says `ChunksWithEmbeddings` is required whenever chunk membership/partitioning/body changes because `chunks` are BM25-visible. If the producer's `public.chunks` table can still contain stale chunks after a projection update, then reading "current rows from public" is not enough. Either make the producer projection delete rows absent from the current canonical document, or materialize the chunk set from the authoritative `documents.canonical_json` for that scope. Without one of those, the stale-chunk P4 acceptance test can pass only by using a synthetic fixture, not the real projection path.

## D3 - Payload Format

**GO with artifact-layout adjustments.** JSONL is the right default for incrementals.

Incrementals are changed-row/change-scope artifacts, not table snapshots. JSONL row objects are easier to inspect, portable across PostgreSQL versions, and fit delete PK tuples and `ReplaceSet` objects naturally. Do not reuse CopyBinary for ordinary incrementals.

Use deterministic JSONL:

- One logical file per table/group + op, e.g. `documents.upsert.jsonl`, `documents.delete.jsonl`, `chunks_with_embeddings.replace_set.jsonl`.
- Upsert lines are full row objects using the same generated-column-excluded column list as P3 `replicated_table_columns`.
- Delete lines are canonical PK objects, not positional arrays.
- Replace-set lines are serialized `ReplaceSet` objects, one per scope.
- Sort rows by PK and replace-set scopes by `(corpus, document_id/table_group)` before writing, so per-file digests are stable.

The committed `artifact.rs` assumes `<table>.copybin`, and `aggregate_payload_digest` looks up `payload_file_name(table)` by apply-order table name. That needs to change before P4. Add either:

- `PayloadFile.file_name`, with aggregate digest computed over the manifest's declared files in `payload.apply_order`; or
- deterministic helpers keyed by `(table_or_group, op, format)`.

For replace-set files, `PayloadFile.table` is currently just a string and `columns` is required. It is acceptable for P4 to set `table` to the group token and `columns = []`, but a cleaner contract is to add an optional `table_group` or use a separate payload-kind discriminator later. Do not overload `<table>.copybin` paths.

## D4 - Incremental Applier

**GO.** Mutating the active physical generation in one cursor-gated transaction is the correct §7.3 model for ordinary incrementals.

Do not build a new generation for P4 incrementals. Resolve the active generation from `jurisearch_control.corpus_state`, apply directly to that physical schema, and rely on PostgreSQL MVCC plus row-level index maintenance. P3's build-new-generation/switch path is for baseline/rebaseline.

Shape:

1. Open one transaction.
2. `SET LOCAL lock_timeout = '5s'`.
3. `pg_try_advisory_xact_lock(APPLY_ADVISORY_LOCK_KEY)`.
4. `SELECT ... FROM jurisearch_control.corpus_state WHERE corpus = $1 FOR UPDATE`.
5. Run idempotency check first.
6. Require `sequence == expected_client_from_sequence`; otherwise reject with `RejectCode::SequenceGap`.
7. Check schema/extensions/bundle, active baseline/generation, embedding fingerprint, builder versions, and previous package id/digest.
8. Apply JSONL files in manifest dependency order.
9. Validate replace-set digests and whole-corpus postconditions.
10. Update `corpus_state.sequence`, `last_package_id`, `last_package_digest`, stamps, and `applied_at`.
11. Commit.

Two implementation details:

- P3 `validate_postconditions` opens a new connection through `corpus_table_digests`, which would not see uncommitted P4 writes. For P4, call `corpus_table_digests_with_client(&mut tx, corpus, DigestSource::Generation { schema })` inside the apply transaction.
- `activate_generation` currently documents itself as the sole writer of `corpus_state`. P4 needs a new storage primitive, e.g. `advance_corpus_cursor_in_active_generation`, and the documentation should be updated to "sole switch writer" vs "incremental cursor writer".

Row-level index maintenance is correct for ordinary incrementals. Keep `IndexBuildContract.row_level_maintenance_only = true`, with `bm25_indexes` and `ivfflat_finalize` empty unless a later phase introduces index DDL in an incremental. Cursor advancement must remain after any required index work.

## D5 - `replace_set` Apply and Stale-Chunk Hazard

**GO with explicit FK validation and group-specific delete rules.** Delete-then-insert is the right apply operation.

For `ChunksWithEmbeddings`, delete from `chunks` by document scope and rely on the recreated `chunk_embeddings(chunk_id) REFERENCES chunks(chunk_id) ON DELETE CASCADE`. For `ZoneUnits`, delete from `zone_units` by document scope and rely on `zone_unit_embeddings` cascade. P3's `build_generation_indexes` recreates foreign keys inside the generation, so this is consistent with the committed generation topology.

Still add a defensive check or test that the active generation has the expected cascade FKs before applying replace-sets. The whole stale-row guarantee depends on those FKs existing after baseline load.

Group rules:

- `ChunksWithEmbeddings`: delete parent `chunks` for the document, insert `chunks`, then insert `chunk_embeddings`.
- `ChunkEmbeddings`: delete only `chunk_embeddings` for the document's chunks, then insert embeddings; do not delete chunks.
- `ZoneUnits`: delete parent `zone_units`, insert `zone_units`, then insert `zone_unit_embeddings`.
- `DecisionZones`, if added: delete/insert the singleton `decision_zones` row for the document.

Verifying `set_digest` after insert is the right §5.3 check. Compute it from the generation rows after apply using the same canonical row hashing as the builder, not from the payload bytes.

## D6 - Idempotency and Ordering

**GO with exact reject semantics.** Use the same package id + package digest identity rule as P3.

Recommended decision tree:

- If current cursor is already at `result_sequence` and `last_package_id` + `last_package_digest` match, return no-op.
- If current cursor is at `result_sequence` but identity differs, reject with `DigestMismatch` or `WrongGeneration`.
- Otherwise require `current sequence == expected_client_from_sequence`.
- If not equal, reject with `SequenceGap`.

For incrementals, do not silently accept `current > result_sequence` unless the exact package identity matches the already-applied cursor. A non-matching ahead/behind state is an ordering failure, not a skip.

Also validate the chain link:

- `manifest.identity.previous_package_id` should match `corpus_state.last_package_id`.
- `previous_package_sha256` should match `corpus_state.last_package_digest`.
- `active_baseline_id` and `active_generation` preconditions should match the cursor row.

## D7 - Concurrent-Ingest Cross-Corpus Correctness

**ADJUST.** Freezing `hi = current_change_seq` inside repeatable read is not sufficient by itself.

The problem is PostgreSQL sequence allocation order versus transaction commit order. An ingest transaction can allocate `change_seq = 100` and remain uncommitted. A later transaction can allocate and commit `change_seq = 101`. A repeatable-read package build can see 101, set `hi = 101`, and not see the uncommitted 100. If 100 commits after the package is cataloged, the next package starts at `low = 101`, so change 100 is permanently skipped.

You need an outbox fence, not just a snapshot.

Practical P4 recommendation:

- Modify `emit_change` once to take a shared transaction advisory lock before inserting into `package_change_log`.
- The incremental builder takes the matching exclusive advisory lock briefly while it freezes `hi` in its build snapshot.
- Because the shared lock is transaction-scoped, the builder waits for in-flight emitters to commit/rollback, and new emitters block until `hi` is frozen.
- After `hi` is frozen, release the exclusive lock and do the longer materialization work from the established snapshot.

This is a central helper change, not eight writer changes, because all hooks already go through `emit_change`. It gives the catalog a real high-water mark: no later-committing row can appear with `change_seq <= hi`.

The corpus-scoped read API is still correct after that. `scopes_changed_for_corpus(corpus, low, high)` prevents `inpi` rows from affecting the `core` package sequence. The missing piece is only the commit-order fence around the global sequence high-water mark.

## Additional Items

**Generated columns and upsert columns:** yes, reuse `replicated_table_columns`. It excludes generated columns and preserves ordinal order. For JSONL, record the same column list in `PayloadFile.columns`, and have the applier reject row objects with missing/extra columns.

**Delete PK naming:** use `primary_key_columns` from storage to emit canonical PK objects. Multi-column keys should be named objects, e.g. `{"corpus":"core","citation_key":"..."}` for `legislation_citation_resolutions`, never positional arrays.

**INV-1 valid_to closes:** current-row upsert does cover `documents.valid_to` updates as long as the writer emits a `documents` scope and the applier updates every non-PK replicated column. The upsert must not be `DO NOTHING` or a partial update keyed only on `source_payload_hash`.

**Compatibility/digest gates:** share the P3 gates. Incrementals should verify signature, client version, schema version + schema bundle digest, extensions, embedding fingerprint, builder versions, per-file digests, aggregate payload digest, preconditions, and postconditions. Keep `postgres_major_min/max` absent for JSONL incrementals unless a physical-format variant is introduced.

**Postcondition cost:** whole-corpus table digests after every incremental are heavy but acceptable for the P4 vertical slice and excellent for proving convergence. You can later gate them by debug/QA or replace with per-scope postconditions, but do not weaken them before P4 acceptance.

**Package sequence:** next package sequence should be `previous.package_sequence + 1`, with `expected_client_from_sequence = previous.package_sequence` and `result_sequence = next`. Do not derive any manifest sequence from `change_seq`.

**No-op empty windows:** define behavior now. If `(low, hi]` has no changed scopes for the corpus, either do not build a package and do not advance package sequence, or build an explicit empty heartbeat package. For P4, I recommend no package; keep the chain for actual diffs.

## Bottom Line

**GO** on JSONL incremental packages applied in one transaction to the active generation.

**ADJUST before coding**:

1. Add the outbox high-water fence; repeatable-read `max(change_seq)` alone can skip late-committing lower sequence rows.
2. Store `included_change_seq_low` and serialize per-corpus package catalog writes.
3. Coalesce changed scopes and implement "widest operation wins".
4. Add/handle `decision_zones` replace-set.
5. Treat document projection scopes as affecting document-owned child sets, especially `ChunksWithEmbeddings`; do not rely on simple upserts for stale-BM25 safety.
6. Generalize artifact file naming and aggregate digest over manifest-declared files.
7. Validate P4 postconditions inside the same apply transaction.

Those changes fit the committed architecture and avoid a wide redo after the first incremental tests.
