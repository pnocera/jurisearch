Verdict: **GO-with-adjustments: use the decoupled strategy, but add the missing `build-zone-units` step and only use the standalone CLI if it is definitely attached to the same producer database.**

I would not run the bundled multi-day `jurisearch-producer update --group jurisprudence --accept-stale-cursor` as the primary path. It is source-correct for ingesting the on-disk delta chain and publishing after enrichment, but it holds the producer update lock for days and gives you no published checkpoint until the very end. More importantly, the source does **not** show producer enrichment materializing `zone_units`: producer `enrich_group` only calls `enrich_zones` for cass/inca (`crates/jurisearch-producer/src/update.rs:685-711`), then the producer embeds pending chunks and zone units (`update.rs:392-400`). The actual `decision_zones` -> `zone_units` derivation is the separate CLI `ingest build-zone-units` path (`crates/jurisearch-cli/src/ingest/pipeline.rs:85-167`). So "enrich then embed zones in the same producer cycle" is only true for zone units already present or invalidated to empty; newly enriched ok rows still need derivation before zone-unit embedding can pick them up.

## Source-grounded assessment

- `--accept-stale-cursor` is the right ingest override for this one-shot. The producer CLI exposes it as a producer-only age-check override (`crates/jurisearch-producer/src/bin/jurisearch_producer.rs:86-105`), and `choose_ingest_mode` uses it only to bypass the stale age error, not malformed cursors (`crates/jurisearch-producer/src/update.rs:540-585`). With the verified contiguous mirror, that is exactly the intended case.
- Delta-only archive selection is efficient and correct: `select_archives_to_process` skips the baseline when `incremental=true` and selects deltas with `compact >= since_compact` (`crates/jurisearch-pipeline/src/ingest/mod.rs:91-109`). The inclusive cursor may re-read the cursor archive, but ingest accounting resume-skips compatible already-ingested members.
- Standalone `ingest enrich-zones` is behaviorally equivalent to the producer enrichment only at the library layer: both delegate to `jurisearch_pipeline::enrich_zones` (`crates/jurisearch-cli/src/ingest/pipeline.rs:11-44`, producer `update.rs:701-711`). That library writes `decision_zones` through the same outbox-coupled storage path.
- `upsert_decision_zones_with_client` emits a document-scoped `decision_zones` replace-set, and emits a `zone_units` replace-set only when a non-derivable status clears existing units (`crates/jurisearch-storage/src/decision_zones.rs:151-258`). It does not create new `zone_units` for ok rows.
- New zone retrieval units are created by `ingest build-zone-units`, which loads derivable ok/non-expired `decision_zones`, calls `derive_zone_unit_rows`, and writes via `replace_zone_units_for_document(..., Some(&outbox))` (`crates/jurisearch-cli/src/ingest/pipeline.rs:85-167`; storage outbox at `crates/jurisearch-storage/src/zone_units.rs:140-245`).
- Zone-unit embeddings are covered by the producer final update: `embed_pending(..., EmbedTarget::ZoneUnits, ...)` is called every non-snapshot producer cycle (`crates/jurisearch-producer/src/update.rs:392-400`), and the zone dense finalize verifies every zone unit has the active embedding before stamping/rebuilding (`crates/jurisearch-storage/src/zone_units.rs:596-700`).
- Package sequencing works cleanly: ordinary incremental publish uses the latest catalog row as the low watermark and emits the next chain link (`crates/jurisearch-package-build/src/incremental.rs:143-149`, `:356-374`; publish at `crates/jurisearch-package-build/src/cycle.rs:162-185`). If current head is seq 2, the text catch-up publishes `core-2-3`; the final enrichment/zone publish then publishes the next link, normally `core-3-4`.

## Recommended run sequence

Keep timers disabled throughout.

1. Deploy the `c6974c1` producer binary and re-verify no producer run is active.

2. Publish the text/data catch-up checkpoint first:

```bash
/usr/local/bin/jurisearch-producer update \
  --config /etc/jurisearch/producer.toml \
  --group jurisprudence \
  --skip-fetch \
  --skip-enrich \
  --accept-stale-cursor
```

Expected result: delta-only ingest from the stale-but-accepted Dec-2025 cursors over the on-disk contiguous chain, chunk embedding for any new/changed chunks, and ordinary incremental publish, expected `core-2-3` if head is still sequence 2. The completed-ingest cursors should advance to the newest on-disk deltas, so future normal timer runs no longer need the override.

3. Run the multi-day Judilibre enrichment outside the producer update cycle, but only against the production writer DB. The current `jurisearch` CLI opens a local managed index via `--index-dir` / `JURISEARCH_INDEX_DIR` (`crates/jurisearch-cli/src/index_runtime.rs:7-42`), not `/etc/jurisearch/producer.toml`. So this is safe only if that index directory is the actual CT110 producer store. If not, add/use a thin producer admin wrapper around the same `enrich_zones` and `build_zone_units_payload` logic before doing this in production.

If the CLI is confirmed to target the producer DB:

```bash
export JURISEARCH_INDEX_DIR=<producer-index-dir-that-opens-CT110-writer-db>

/usr/local/bin/jurisearch ingest enrich-zones --source cass --order oldest --concurrency <safe-N>
/usr/local/bin/jurisearch ingest enrich-zones --source inca --order oldest --concurrency <safe-N>

/usr/local/bin/jurisearch ingest build-zone-units
```

Use the same PISTE environment the producer service uses. Re-run the two `enrich-zones` commands as needed after interruptions; attempted rows are persisted in `decision_zones`, and status-null candidates remain retryable. `build-zone-units` is idempotent and derives only missing/stale units unless `--rebuild` is passed; do not pass `--rebuild` for this catch-up unless you deliberately want a full re-derive.

4. Final producer publish after enrichment and zone-unit derivation:

```bash
/usr/local/bin/jurisearch-producer update \
  --config /etc/jurisearch/producer.toml \
  --group jurisprudence \
  --skip-fetch
```

Do **not** use `--skip-enrich` here unless you have separately verified there are no remaining `status IS NULL` cass/inca candidates. Letting enrichment run makes the final cycle retry any missed/null candidates, then embed pending `zone_units`, and publish the package containing `decision_zones`, `zone_units`, and `zone_unit_embeddings`. Expected package is the next incremental after the text checkpoint, normally `core-3-4`.

## Success signals

- After step 2: producer exits success; jurisprudence ingest journals complete; completed-ingest cursor per source is at the June-2026 archive tail; served manifest advances by one sequence, expected `head_sequence=3` / `core-2-3`.
- After enrichment: cass/inca `decision_zones` coverage shows the intended attempted set is no longer status-null, accepting expected `not_found` for pre-2022 decisions.
- After `build-zone-units`: derivable ok rows have materialized `zone_units`; rerunning without `--rebuild` should report no or near-no derivable work.
- After final producer update: zone-unit embedding coverage is complete; served manifest advances again, expected `head_sequence=4` / `core-3-4`; package catalog row is `published`; timers remain disabled until this is verified.

## Risk notes

- The main correctness risk is running the standalone CLI against the wrong database. The CLI path is equivalent only if it opens the producer database that the package builder reads. If this deployment is purely external-DB producer config with no matching managed `JURISEARCH_INDEX_DIR`, do not use the CLI directly; add a producer-side maintenance subcommand for `enrich-zones` and `build-zone-units`.
- `--skip-fetch` is appropriate for the text catch-up because the mirror is already verified contiguous through end-June and the goal is to ingest those files. The next normal timer run should fetch again and pick up newer DILA files.
- Bundled is not unsafe for existing text data, but it is less robust operationally and, without an explicit zone-unit derivation phase, does not fully satisfy "current including zones."

Final recommendation: **Decoupled plus explicit zone-unit derivation.** It preserves work across interruptions, gets a durable text checkpoint quickly, and still produces a final signed package with enrichment and zone vectors before timers are re-armed.
