Verdict: **GO-with-adjustments.** The core design is right: zone-unit derivation belongs in `jurisearch-pipeline`, the producer must call it after Judilibre enrichment and before zone-unit embedding, and the outbox/package-window model will capture those derived rows in the same publish. I would adjust the gating, reporting, and one idempotency edge before coding.

## Required Adjustments

1. **Move the derivation into `jurisearch-pipeline`, not the producer.**

   `jurisearch-pipeline` is the correct home. Its crate docs already define it as the reusable producer-facing home for ingest/enrich/embed APIs over `DbClientSource` (`crates/jurisearch-pipeline/src/lib.rs:1-14`), and the dependency direction is already `jurisearch-cli -> jurisearch-pipeline`, never the reverse. Moving CLI-only derivation there removes the architectural gap without giving the producer a CLI dependency.

   Add a new module, preferably `crates/jurisearch-pipeline/src/build_zone_units.rs`, and publicly export:

   - `pub const ZONE_UNIT_BUILDER_VERSION: &str = "zone-units:v1";`
   - `pub const BUILD_ZONE_UNITS_PAGE_SIZE: u32 = 500;`
   - `pub fn derive_zone_unit_rows<'a>(...) -> Vec<ZoneUnitRow<'a>>`
   - `pub struct BuildZoneUnitsRequest { pub limit: Option<u32>, pub rebuild: bool }`
   - `pub struct BuildZoneUnitsOutcome { pub decisions_derived: u64, pub zone_units_written: u64, pub coverage: serde_json::Value }`
   - `pub fn build_zone_units(db: &impl DbClientSource, req: BuildZoneUnitsRequest) -> Result<BuildZoneUnitsOutcome, BuildZoneUnitsError>`

   The CLI function `derive_zone_unit_rows` currently depends only on `ZoneUnitRow`, `serde_json::Value`, and `ZONE_UNIT_BUILDER_VERSION` (`crates/jurisearch-cli/src/ingest/pipeline.rs:47-83`), so this extraction is clean. `ZoneUnitRow` is already public in storage (`crates/jurisearch-storage/src/zone_units.rs:129-137`).

2. **Use a typed pipeline error; do not overload `EnrichError`.**

   Add `BuildZoneUnitsError` beside `IngestError`, `EnrichError`, and `EmbedError` in `crates/jurisearch-pipeline/src/error.rs`. The implementation will mostly map `StorageError` and JSON parse failures through the existing `storage_error_object` / `dependency_unavailable` helpers, just as the other pipeline entrypoints do.

   In `crates/jurisearch-producer/src/error.rs`, add a producer variant such as:

   ```rust
   #[error("zone-unit derivation failed: {0}")]
   ZoneUnits(#[from] jurisearch_pipeline::BuildZoneUnitsError),
   ```

   Give it a distinct class, for example `zone-units-failed`, or intentionally map it to `enrich-degraded` if alert taxonomy must stay smaller. I prefer the distinct class because this phase is deterministic DB derivation, not Judilibre network enrichment.

3. **Run derivation when the group contains cass/inca and the run is not `snapshot_only`; do not gate it on `skip_enrich`.**

   Placement is load-bearing and correct: after Phase 4 enrichment and before Phase 5 embedding. Producer `enrich_group` writes `decision_zones` (`crates/jurisearch-producer/src/update.rs:685-711`), and producer embedding already calls `EmbedTarget::ZoneUnits` after chunk embedding (`update.rs:392-400`). The missing step is exactly between them.

   Gating should be:

   ```rust
   let zone_units = if !options.snapshot_only
       && sources.iter().any(|s| matches!(s, ArchiveSource::Cass | ArchiveSource::Inca))
   {
       Some(jurisearch_pipeline::build_zone_units(
           &db,
           BuildZoneUnitsRequest { limit: None, rebuild: false },
       )?)
   } else {
       None
   };
   ```

   It should still run with `--skip-enrich`: a prior interrupted or operator-run enrichment can leave `decision_zones` rows with missing/stale `zone_units`. `--skip-enrich` should mean "do not call Judilibre", not "skip deterministic derivation from already-cached zones." Gating by cass/inca avoids pointless scans on legislation-only cycles; the storage selector also restricts to cass/inca (`crates/jurisearch-storage/src/zone_units.rs:276-305`), so always-running would be mostly harmless but less intentional.

4. **Do not require a new durable `RunPhase` for the minimal slice.**

   Adding `RunPhase::ZoneUnitsDerived` is possible, but it touches serialized checkpoint schema (`crates/jurisearch-producer/src/cursors.rs:62-72`) without buying much. The update checkpoint does not currently resume by skipping completed phases; rerunning derivation is idempotent. For the minimal slice, keep `RunPhase::Enriched` after enrichment/derivation or add the derivation call immediately before setting `RunPhase::Embedded`.

   Do add `zone_units: Option<BuildZoneUnitsOutcome>` or a small producer-local report struct to `UpdateReport` (`crates/jurisearch-producer/src/update.rs:132-152`) and include counts in the CLI JSON summary (`crates/jurisearch-producer/src/bin/jurisearch_producer.rs:300-325`, `:430-456`). That gives operators the visibility they need without making checkpoint compatibility part of this patch.

5. **Resolve or consciously accept the zero-derived-units edge before enabling this every timer run.**

   The existing selector treats an ok/non-expired `decision_zones` row as derivable when no `zone_units` exist for that document (`crates/jurisearch-storage/src/zone_units.rs:247-305`). `derive_zone_unit_rows` deliberately skips blank/missing fragment text (`crates/jurisearch-cli/src/ingest/pipeline.rs:57-83`). Therefore, if an `ok` row can normalize to zero non-empty fragments, `replace_zone_units_for_document(..., &[])` clears units, but the row remains selected on every future run. That would emit repeated `zone_units` replace-set outbox rows and cause timer churn.

   Before coding, either prove from `normalize_judilibre_zones` / Judilibre invariants that `status='ok'` always produces at least one non-empty derivable fragment, or add an explicit "derived empty" marker. Minimal marker options include a small `zone_unit_derivation` manifest table or a storage-side change to let `load_derivable_decision_zones_json_with_client` distinguish "never derived" from "derived to zero rows." Do not hide this by just ignoring empty rows in the producer loop, because then the same candidate will be retried forever.

6. **Preserve CLI output shape by delegating, not rewriting.**

   Refactor `crates/jurisearch-cli/src/ingest/pipeline.rs::build_zone_units_payload` to open the local managed index as today, call `jurisearch_pipeline::build_zone_units`, then wrap the result with the same JSON keys:

   - `schema_version`
   - `command: "ingest build-zone-units"`
   - `index_dir`
   - `builder_version`
   - `rebuild`
   - `decisions_derived`
   - `zone_units_written`
   - `coverage`

   Move the existing `derive_zone_unit_rows_handles_multi_fragment_and_skips_empty` test import to `jurisearch_pipeline::derive_zone_unit_rows` and `jurisearch_pipeline::ZONE_UNIT_BUILDER_VERSION` (`crates/jurisearch-cli/src/tests.rs:454-479`). Better yet, move that unit test into the pipeline crate and leave only CLI payload-shape coverage in the CLI crate.

## Correctness Checks

1. **Outbox and package-window correctness: OK.**

   `replace_zone_units_for_document_with_client` writes the replacement set in one transaction and emits exactly one document-scoped `zone_units` replace-set outbox row when an `OutboxContext` is provided (`crates/jurisearch-storage/src/zone_units.rs:140-245`). The outbox writer allocates `package_change_log.change_seq` under the shared advisory fence (`crates/jurisearch-storage/src/outbox.rs:143-201`).

   The ordinary incremental builder freezes `hi = current_change_seq_with_client(...)` after taking its repeatable-read snapshot (`crates/jurisearch-package-build/src/incremental.rs:178-190`) and then includes scopes in `(lo, hi]`. Since the producer will run derivation and zone-unit embedding before Phase 6 publish, those outbox rows commit before `hi` is frozen and are included in the same package. The `ingest_run_id` being `build-zone-units-...` instead of the update run id is fine; it is metadata on the ledger row, not the package window key.

   There is no double-counting problem: the incremental builder coalesces changed scopes by table/document. If derivation emits `zone_units` and embedding later emits `zone_units` plus `zone_unit_embeddings`, the final package should contain the current materialized state for those document scopes.

2. **Interrupt/resume behavior: OK with the zero-row caveat above.**

   The derivation selector pages by `document_id` and only selects ok/non-expired rows with absent or stale units (`crates/jurisearch-storage/src/zone_units.rs:247-305`). `replace_zone_units_for_document` is idempotent: delete all units for a document, insert the newly derived set, and emit one replace-set (`zone_units.rs:140-245`). If the producer is interrupted after some documents are derived, rerun starts again and skips documents whose `text_hash` and `zone_unit_builder_version` now match, except for the zero-derived-units edge.

   Stale embeddings are handled correctly. `zone_unit_embeddings.zone_unit_id` references `zone_units(zone_unit_id) ON DELETE CASCADE` (`crates/jurisearch-storage/src/migrations.rs:553-554`), so replacing units removes obsolete embeddings. The subsequent `EmbedTarget::ZoneUnits` pass inserts current embeddings, stamps `zone_units.embedding_fingerprint`, emits outbox rows for `zone_units` and `zone_unit_embeddings`, and finalizes the zone dense index (`crates/jurisearch-storage/src/zone_units.rs:414-545`, `:596-700`).

3. **Lifetime of borrowed fragment text: OK.**

   The existing function returns `ZoneUnitRow<'a>` values borrowing text from the page `Value` (`crates/jurisearch-cli/src/ingest/pipeline.rs:47-83`). The rows are immediately passed to `replace_zone_units_for_document...` inside the same loop iteration (`pipeline.rs:129-138`). Moving this into `jurisearch-pipeline` preserves that lifetime shape. Do not store `ZoneUnitRow` across page iterations.

4. **Version coupling: OK, but move the const to the shared crate.**

   The builder-version is part of the derivation staleness predicate (`load_derivable_decision_zones_json_with_client` compares `u.zone_unit_builder_version` to the supplied builder literal). Keeping `ZONE_UNIT_BUILDER_VERSION` in the CLI is now wrong because the producer will become the authoritative derivation runner. Put it in `jurisearch-pipeline` and re-export it so both CLI and producer use the same value.

## Minimal Correct Slice

1. Add `crates/jurisearch-pipeline/src/build_zone_units.rs`.

   Include:

   - `ZONE_UNIT_BUILDER_VERSION`
   - `BUILD_ZONE_UNITS_PAGE_SIZE`
   - `BuildZoneUnitsRequest`
   - `BuildZoneUnitsOutcome`
   - `derive_zone_unit_rows`
   - `build_zone_units`

   Implementation should use a fresh `db.client()?`, `producer_run_id("build-zone-units")`, `OutboxContext::new(&run_id, CURRENT_SCHEMA_VERSION)`, `load_derivable_decision_zones_json_with_client`, `replace_zone_units_for_document_with_client`, and `zone_retrieval_coverage_with_client`.

2. Update `crates/jurisearch-pipeline/src/lib.rs`.

   Export the new public API and import the storage symbols currently missing from the pipeline prelude:

   - `ZoneUnitRow`
   - `load_derivable_decision_zones_json_with_client`
   - `replace_zone_units_for_document_with_client`
   - `zone_retrieval_coverage_with_client`

   Add `BuildZoneUnitsError` to the public error exports.

3. Refactor `crates/jurisearch-cli/src/ingest/pipeline.rs`.

   Remove local `derive_zone_unit_rows` and the local build loop. Keep `build_zone_units_payload`, but delegate to the pipeline and preserve the response JSON shape. Remove the CLI-local `ZONE_UNIT_BUILDER_VERSION` and `BUILD_ZONE_UNITS_PAGE_SIZE` constants from `crates/jurisearch-cli/src/main.rs` or replace references with the pipeline exports.

4. Update producer orchestration in `crates/jurisearch-producer/src/update.rs`.

   Add a helper, for example:

   ```rust
   fn derive_zone_units_if_applicable(
       db: &impl DbClientSource,
       sources: &[ArchiveSource],
       snapshot_only: bool,
   ) -> Result<Option<BuildZoneUnitsOutcome>, ProducerError>
   ```

   Call it after enrichment and before `embed_pending(..., EmbedTarget::Chunks, ...)`. Store the outcome in `UpdateReport` and include it in both ordinary incremental and rebaseline report construction. Do not call it on `snapshot_only`.

5. Update `crates/jurisearch-producer/src/bin/jurisearch_producer.rs`.

   Add `zone_units` counts to the `update` and non-snapshot `rebaseline` JSON output. For `--from-db`, report `null` or omitted counts; do not derive because snapshot-only intentionally publishes the DB as-is after preflight.

6. Tests.

   Minimal tests:

   - Move/duplicate the pure derivation test into `jurisearch-pipeline`.
   - Add a pipeline/storage integration test that inserts an ok `decision_zones` row with zones JSON, runs `build_zone_units`, and asserts `zone_units` rows plus a document-scoped outbox row exist.
   - Add a producer orchestration test around a cass/inca run that stubs or seeds an already-enriched decision, runs the new phase, then verifies `embed_pending(ZoneUnits)` sees the derived units before publish. If a full producer-cycle test is too expensive, a focused helper test for the new phase is acceptable for the minimal slice.

## Deferred Work

- A standalone producer subcommand for `build-zone-units` is useful but not required for steady-state once the update cycle includes the phase.
- More detailed dashboard/run-record rendering of derivation counts can follow after the JSON summary is present.
- Cursor/checkpoint resume semantics do not need to change for this patch.

Final verdict: **GO-with-adjustments.** The proposed extraction and producer Phase 4.5 are the right fix. Apply the adjusted gating, use a proper pipeline error/report API, preserve CLI output via delegation, and resolve the zero-derived-units idempotency edge before turning this on in the producer timer path.
