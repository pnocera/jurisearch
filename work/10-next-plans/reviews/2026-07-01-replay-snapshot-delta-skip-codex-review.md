NIT [crates/jurisearch-producer/src/update.rs:546](/home/pierre/Work/jurisearch/crates/jurisearch-producer/src/update.rs:546): The new policy comment says full-scan cycles include a "cold-or-stale cursor", but the source above it still fail-closes on stale cursors: `choose_ingest_mode` returns `ProducerError::IngestCursorStale` at [crates/jurisearch-producer/src/update.rs:481](/home/pierre/Work/jurisearch/crates/jurisearch-producer/src/update.rs:481) rather than resolving `incremental=false`. The implementation is correct and matches the safety model, but the comment now overstates when refresh occurs. Fix: change the parenthetical to "rebaseline / pending baseline / cold cursor" and, if useful, add that stale cursors error before ingest.

I did not find any functional blockers or warnings in the reviewed diff.

The skip path leaves the cached snapshot intact: `maybe_refresh_replay_snapshot(client, false)` returns `Ok(None)` and only the real refresh path reaches storage's `store_replay_snapshot`, so the existing `index_manifest['replay_snapshot']` row is not cleared. The ingest sites preserve the `run_status == Completed` gate and add the policy as an extra condition through `maybe_refresh_replay_snapshot`; `EmbedTarget::Chunks` consumes the new policy, while `EmbedTarget::ZoneUnits` still returns `replay_snapshot: None` and has no replay-cache field.

The producer's axis is also correct. `ingest_one` passes `refresh_replay_snapshot: !mode.incremental` and stores the same value in `IngestJournalCoordinate.full_scan`; `run_update_inner` then ORs the fresh in-memory journals to drive the chunk-embed refresh. Pending baseline and cold-cursor modes resolve to `incremental=false`; fresh completed cursors resolve to delta-only `incremental=true`; stale cursors still fail closed before ingest. The embed policy is not derived from inserted embeddings or `NoResults`, and a mixed multi-source cycle refreshes if any source full-scanned.

Non-producer callers preserve prior behavior by explicitly passing `refresh_replay_snapshot: true` for CLI archive ingest and CLI chunk/zone embedding. I found no `Default` impl or struct-update construction that could silently default the new fields to `false`. The `#[serde(default)]` on `IngestJournalCoordinate.full_scan` is backward-compatible for old persisted JSON, and the refresh decision for a new run is recomputed from newly produced journals rather than loaded from a checkpoint default.

The new tests are meaningful for the pure policy surface: the pipeline test covers the 2x2 policy/env truth table and the skipped report shape, and the producer tests pin the mode-to-`!incremental` derivation plus the OR across source journals. The remaining coverage gap is the expected DB-gated end-to-end case: proving that a delta-only skipped cycle leaves the existing manifest row untouched while a full refresh updates it.

Validation run during review:

```text
cargo test -p jurisearch-pipeline replay_policy_tests --lib
cargo test -p jurisearch-producer full_scan
```

Both commands passed. An earlier attempt to pass multiple cargo test filters in one command failed with Cargo argument parsing before tests ran; it did not exercise code.

VERDICT: GO
