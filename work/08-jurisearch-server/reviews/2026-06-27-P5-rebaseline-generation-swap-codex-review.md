# P5 Code Review: Re-baseline and Generation Swap

## Findings

### BLOCKER - Building-generation cleanup is not actually locked and can drop an in-progress apply

`apply_media_package` unconditionally calls `reset_building_generation` before creating the manifest-declared generation (`crates/jurisearch-syncd/src/apply.rs:187`). The helper then does `SELECT ... FOR UPDATE`, `DROP SCHEMA IF EXISTS ... CASCADE`, and deletes the registry row on the caller's plain autocommit client (`crates/jurisearch-storage/src/generations.rs:523-555`).

That `FOR UPDATE` lock is released at the end of the `SELECT` statement; it is not held through the `DROP` or registry delete. More importantly, any `state = 'building'` row is treated as resettable, with no owner token, stale marker, explicit retry mode, or advisory lock proving that the row is a failed half-built generation rather than another syncd process actively loading the same deterministic generation. Two concurrent applies of the same re-baseline can therefore race as:

1. applier A creates `core_g0002` and starts copying/building;
2. applier B enters `reset_building_generation`, sees A's `building` row, and drops `jurisearch_server_core_g0002` out from under A.

That violates the P5 requirement that `DROP SCHEMA ... CASCADE` be confined to locked cleanup of a registry-confirmed retired or half-built private generation. It is not on the switch path, which is good, but this cleanup path is not locked tightly enough and does not prove the generation is abandoned.

Recommended fix: make deterministic-generation retry cleanup an explicit, locked operation. At minimum, take a generation-scoped advisory lock before reset/create and hold it until the build attempt no longer has a live `building` schema; wrap the registry lookup, drop, delete, and re-create decision in one transaction; and refuse to reset a `building` row unless the caller can prove it owns that failed attempt or that the row is stale by a persisted attempt token/started-at marker. If that is too large for P5, remove the automatic reset from `apply_media_package` and return a retryable "building generation already exists" error with a separate operated cleanup command.

### WARN - The global dense manifest still leaves cross-corpus dense isolation unproven

The requested adjustment to avoid pre-switch dense metadata mutation is implemented: `apply_media_package` only assembles dense manifest entries before validation (`crates/jurisearch-syncd/src/apply.rs:206-212`), and `activate_generation_with_guard` writes them inside the same transaction as cursor update and view rebuild (`crates/jurisearch-storage/src/generations.rs:1062-1084`).

However, the rows are still global `index_manifest` keys (`embedding` and `zone_embedding`) written with `ON CONFLICT (key)` (`crates/jurisearch-storage/src/generations.rs:469-476`). Dense retrieval reads those same global keys for probe defaults (`crates/jurisearch-storage/src/retrieval/sql.rs:21-28`, `crates/jurisearch-storage/src/retrieval/hybrid.rs:19-22`, `crates/jurisearch-storage/src/zone_retrieval.rs:218-223`). A core re-baseline with different dense settings will therefore change the dense metadata seen by another active corpus even though that corpus's generation and cursor row are untouched.

The current P5 test installs `inpi`, but it does not give `inpi` distinct dense settings or assert dense-query behavior after the core swap. So it proves view/cursor/app survival, not full dense metadata isolation.

Recommended fix: either scope `index_manifest` by corpus/generation and have retrieval resolve dense metadata through `corpus_state.active_generation`, or explicitly narrow the P5 acceptance notes to "global dense metadata remains shared" and add a test documenting the supported same-dense-settings case. If the claim is that every installed corpus remains independently queryable across different dense configurations, this needs the scoped manifest fix now.

### WARN - The deterministic generation-label test has a false-green hole for the original fresh-client bug

The implementation itself adopts `manifest.identity.generation` on apply (`crates/jurisearch-syncd/src/apply.rs:177-195`), which addresses the design adjustment. But the new loopback test does not actually prove that specific bug stays fixed. The test applies the first baseline to the client, skips one incremental, then applies a re-baseline whose producer label is `core_g0002` (`crates/jurisearch-package-build/tests/rebaseline_loopback.rs:180-190`, `250-270`). If the applier regressed back to `next_generation_counter`, that client would also allocate `core_g0002` because it already has `core_g0001`, and the post-re-baseline incremental would still pass (`crates/jurisearch-package-build/tests/rebaseline_loopback.rs:363-404`).

The critical failure mode from the design note was a fresh client jumping straight to a later re-baseline: a local counter would allocate `core_g0001`, while the producer's later incremental expects the re-baseline label. The current test does not cover that state.

Recommended fix: extend `rebaseline_loopback` with a second, freshly migrated client that has no `corpus_state` and no generation registry for `core`; apply the same `rb_art` directly; assert `corpus_state.active_generation = 'core_g0002'`; then apply the post-re-baseline incremental and assert it succeeds. That version would fail if `apply_rebaseline` ever returned to local counter allocation.

## Notes

The core shape is otherwise consistent with the P5 design: re-baseline build is a full media package that advances the existing per-corpus sequence, does not reject changed embedding fingerprints or builder versions, chain-links through the producer catalog, and the switch transaction includes the advisory lock, low `lock_timeout`, cursor guard, active generation update, dense manifest write, and view rebuild. The first-baseline, exact-previous, and forward-supersession cursor guards are distinct in `CursorGuard`, and the retired-generation cleanup path uses a transaction and a retired-state registry check.

VERDICT: FIXES_REQUIRED
