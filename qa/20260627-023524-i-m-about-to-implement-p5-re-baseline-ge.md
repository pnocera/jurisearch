# P5 Re-baseline + Generation Swap Design Validation

Overall: **GO with adjustments**. The proposed long-build/short-switch shape matches the committed storage topology, and the sequence model should stay in the same per-corpus package lineage. But I would not implement it exactly as proposed until you fix two source-level issues:

- A self-sufficient rebaseline applied to a fresh client can diverge from the producer's `manifest.identity.generation` if the applier uses `next_generation_counter`; the current P4 incremental applier then rejects later incrementals because it checks `active_generation`.
- The current baseline apply path writes global `index_manifest` rows before activation. That is tolerable for the P3 first-load loopback, but it is wrong for P5 rebaseline because it mutates global read metadata before the short switch and is not per-corpus.

## Q1 - Sequence and Cursor Model

**GO.** A rebaseline should advance the existing per-corpus sequence from `N` to `N+1`; do not reset to a new lineage.

That fits the committed package model:

- `PackageKind::Rebaseline` already means a full reissue that scope-replaces one corpus's server set.
- `PackageSequence` is the per-corpus cursor space; `ChangeSeq` remains only the outbox/catalog audit coordinate.
- P4 incrementals already chain from `previous.package_sequence` to `previous + 1` and link by previous package id/digest.

Forward supersession does not violate INV-2 if you treat rebaseline as a full chain node, not as an incremental gap skip. INV-2 means ordinary incrementals are gap-free and never skipped within an incremental chain. A signed rebaseline is a self-contained package that supersedes missing incrementals by replacing the whole corpus state and advancing the cursor to its own `result_sequence`.

Recommended manifest/catalog shape:

- `package_kind = Rebaseline`
- `from_sequence = previous_head.package_sequence`
- `to_sequence = result_sequence = previous_head.package_sequence + 1`
- `previous_package_id` / `previous_package_sha256` = previous catalog head
- `baseline_id = new baseline id`
- `requires_empty_generation = true`
- `rollback_policy = KeepPreviousGenerationUntilValidated`
- catalog row kind `rebaseline`
- catalog window `(included_change_seq_low, included_change_seq_high] = (previous.included_change_seq_high, hi]`

Use the outbox fence exactly like P4: acquire exclusive fence, start the repeatable-read snapshot, make `current_change_seq_with_client` the first read to freeze `hi`, release the fence, then COPY/digest/index-inventory from that snapshot. The rebaseline does not need the changed scopes, but it still needs `hi` so the next incremental starts after the full snapshot it shipped.

Forward supersession guard:

- Accept `current_cursor IS NULL`.
- Accept `current_cursor < result_sequence`.
- Treat `current_cursor == result_sequence` as idempotent only if package id and package digest match.
- Reject `current_cursor == result_sequence` with a different identity.
- Reject `current_cursor > result_sequence`.

That handles a fresh client, a long-offline client, and a client partway through the old incremental chain.

Do not require the installed client's current `last_package_id` to equal the rebaseline's `previous_package_id` in the forward-supersession case. That would defeat the point of skipping retained incrementals. The previous link is still useful for the producer catalog and remote chain, but a rebaseline apply to an older client should be gated by schema/entitlement/signature/digest/postconditions, not by exact previous package identity.

## Q2 - `CursorGuard` vs Separate Activation Function

**GO.** Extend `activate_generation` with a cursor guard enum rather than creating a separate activation implementation.

The switch mechanics are identical for baseline and rebaseline:

- package apply advisory lock
- `lock_timeout`
- target generation must be `building`
- lock the current `corpus_state` row with `FOR UPDATE`
- retire only this corpus's old active generation
- mark the new generation active
- update `corpus_state`
- rebuild stable views
- commit

Duplicating that in `activate_rebaseline` would create two switch implementations for the same invariant.

Use a type like:

```rust
pub enum CursorGuard {
    FirstBaseline,
    ExactPrevious(i64),
    RebaselineForward { result_sequence: i64 },
}
```

Semantics:

- `FirstBaseline`: require no `corpus_state` row. This preserves the P2/P3 fix that `None` cannot clobber an installed corpus.
- `ExactPrevious(n)`: require current sequence exactly `n`. This remains the baseline-after-none / ordinary controlled switch behavior.
- `RebaselineForward { result_sequence }`: require current sequence absent or less than `result_sequence`.

There is no new race in the `FOR UPDATE` read if you keep the apply advisory lock and cursor row lock in the same activation transaction. A concurrent incremental/rebaseline that advances the cursor before the switch will be observed by the guard. A duplicate same-package rebaseline may still do the long build twice; the second activation should fail cleanly if the first already advanced the cursor. That is acceptable, though you can improve UX by rechecking idempotency under the switch lock and returning a no-op for the exact same package.

## Critical Gap - Manifest Generation vs Client-Local Generation

**ADJUST before coding.** The proposed "new generation counter current head + 1" is dangerous with forward-supersession to a fresh client.

Current P3 behavior:

- `build_baseline` writes `identity.generation = core_g0001`.
- `apply_baseline` ignores that label and uses `next_generation_counter` locally.
- For the first baseline this happens to match: a fresh client also creates `core_g0001`.

For P5:

- Producer head might be `core_g0004`; `build_rebaseline` would write `identity.generation = core_g0005`.
- A fresh client applying that rebaseline has no local generation registry, so `next_generation_counter` would allocate `core_g0001`.
- P4 `apply_incremental` checks `manifest.apply.preconditions.active_generation` against `corpus_state.active_generation`.
- The next producer incremental after the rebaseline will likely carry `active_generation = core_g0005`; the fresh client would have `core_g0001` and reject a valid package.

Pick one invariant now:

1. **Deterministic manifest generation labels.** Media applies create the physical generation named by `manifest.identity.generation`, failing if that schema/registry row already exists with a different identity. This makes later incremental `active_generation` preconditions work even for fresh clients applying a later rebaseline.

2. **Stop using physical generation labels as package-chain preconditions.** Keep generation names client-local and have incrementals precondition on `baseline_id`, previous package id/digest, schema version, embedding fingerprint, and builder versions, not `active_generation`.

I prefer option 2 long-term because generation names are a local storage implementation detail. But it is wider because P4 already enforces `active_generation`. For the smallest P5 vertical slice, option 1 is probably less invasive: make rebaseline apply use the manifest-declared generation label, not `next_generation_counter`, and keep `next_generation_counter` only for retry cleanup/fallback paths where the package does not carry a deterministic generation label.

Do not ship "fresh client can apply a rebaseline" plus "incrementals require producer generation label" plus "applier allocates local next generation" together. That combination will break catch-up immediately after the rebaseline.

## Q3 - `DROP SCHEMA ... CASCADE` on Retired Generation Cleanup

**GO with documentation wording tightened.** The current `drop_retired_generation` is safe for the intended cleanup path, but the acceptance sentence is stricter than the code.

The source already does the important safety work:

- It looks up `(corpus, generation)` in `jurisearch_control.generation_registry`.
- It requires `state = 'retired'`.
- It locks that registry row `FOR UPDATE`.
- It uses the stored `physical_schema`.
- It drops only after the switch has moved reads off the schema.

So yes: `DROP SCHEMA ... CASCADE` on an already-retired private generation schema is not the dangerous pattern the design rejected. The rejected pattern is dropping/reloading the active server schema as the operated switch mechanism.

However, the plan's acceptance wording says "`DROP SCHEMA ... CASCADE` appears only on a documented disaster-recovery path, never the operated one." The committed code documents it as operated async cleanup, not disaster recovery. If the P5 test/review treats that sentence literally, the current code fails already.

Recommendation: update the P5 acceptance wording to:

> `DROP SCHEMA ... CASCADE` is never used for the apply/switch path; it is allowed only for locked cleanup of a registry-confirmed retired private generation schema, with retired fallback if cleanup fails.

Then add a test that `drop_retired_generation` refuses active/building/missing generations and leaves the registry/schema untouched on failure.

## Q4 - Schema-Bump Scope

**GO.** For the P5 vertical slice, cover re-embed and builder-bump rebaselines with unchanged storage schema, and require clients to be migrated before applying a package with a new schema version.

That matches the current syncd gates:

- `check_schema_compatibility` requires the local `schema_migrations` max to equal the package `schema_version`.
- It also checks the schema bundle digest.
- There is no artifact-carried migration executor yet.

So P5 should not pretend to carry breaking DDL. It can still use the manifest's `schema_migration_bundle_digest` as a compatibility proof, but true schema migration delivery should stay deferred.

Concretely:

- Re-embed/builder bump: P5 supported.
- Corpus-rewriting data migration with same storage schema: P5 supported if the producer's `public` state already reflects it and the rebaseline payload is full.
- Storage schema bump: client must migrate first; package apply rejects otherwise.
- Artifact-carried DDL: later phase.

## Q5 - Per-Corpus Isolation and App Survival

**ADJUST for global index metadata.** The generation/view/app isolation is mostly correct, but `index_manifest` is still global.

Good source facts:

- `create_generation_load_tables` creates only the per-corpus physical generation schema.
- `REPLICATED_TABLES` excludes `jurisearch_app`, `jurisearch_control`, `schema_migrations`, `index_manifest`, and ingest/outbox tables.
- `activate_generation` retires only rows where `corpus = $1`.
- `rebuild_server_views` unions all active generation schemas, so another installed corpus remains present in the stable views.
- `jurisearch_app` is a separate schema and is never part of generation creation/drop.
- `drop_retired_generation` drops only a registry-confirmed retired physical schema.

The isolation gap is `upsert_generation_dense_manifest`: it writes global `index_manifest` keys `embedding` and `zone_embedding`. The current baseline apply calls `write_dense_index_manifests` before activation. For P5 that means a rebaseline of `core` can alter global dense query metadata while `core` still reads the old generation, and it can affect another active corpus if `inpi` has different embedding/index settings.

Do not blindly share `apply_baseline`'s current order for rebaseline. For P5, either:

- move dense `index_manifest` updates into the same short activation transaction, after postcondition validation and before cursor update; or
- make dense metadata generation/corpus-scoped and have retrieval resolve it from `corpus_state.active_generation`.

The second is architecturally cleaner, but larger. For the vertical slice, moving the update into the activation transaction is the minimum. It still does not fully solve multi-corpus different-embedding settings, but it prevents pre-switch global mutation and rollback leakage.

If your P5 test installs `core` and `inpi` with the same dense settings, it can prove the view/app/generation isolation. It will not prove per-corpus dense metadata isolation. Add that as an explicit deferred risk unless you fix `index_manifest` now.

## Builder Recommendations

**ADJUST.** Do not copy/paste `build_baseline`; extract a shared full-snapshot media builder with kind-specific identity.

Shared media builder core:

- Acquire per-corpus package build lock.
- Read latest catalog row.
- Acquire outbox fence.
- Start repeatable-read read-only transaction.
- First read `hi = current_change_seq_with_client`.
- Release fence.
- Compute producer postcondition digests.
- COPY each replicated table using `baseline_copy_out_select`.
- Query BM25 index names and schema bundle digest.
- Commit snapshot.
- Build manifest and catalog row.

Baseline-specific:

- Requires no previous catalog row or rejects if one exists.
- `sequence = 1`
- `package_kind = Baseline`
- `previous_package_* = None`
- `baseline_id` from params
- `generation = core_g0001` if you keep deterministic labels
- `included_change_seq_low = 0`

Rebaseline-specific:

- Requires a previous catalog row.
- `from_sequence = prev.package_sequence`
- `to_sequence = prev.package_sequence + 1`
- `package_kind = Rebaseline`
- `previous_package_* = prev`
- new `baseline_id`
- new generation identity per the decision above
- `included_change_seq_low = prev.included_change_seq_high`
- compatibility stamps may change for embedding fingerprint and builder versions

Unlike `build_incremental`, a rebaseline must **not** reject changed embedding fingerprint or builder versions. That is the point of the package kind.

## Applier Recommendations

**ADJUST.** Share the load/index/validate core with baseline, but split the package-kind and cursor semantics.

The current `apply_baseline` has private helpers that P5 will want:

- signature/manifest read
- client/schema/extension/copy-binary gates
- per-file digest verification
- generation load
- index build and index contract validation
- postcondition validation

Extract something like:

```rust
apply_media_package(kind, cursor_guard)
```

or internal helpers:

- `read_verified_manifest`
- `check_media_gates`
- `verify_payload_integrity`
- `load_media_payload_into_generation`
- `build_validate_media_generation`
- `activate_media_generation`

Then keep public functions:

- `apply_baseline`: accepts only `Baseline`, guard `FirstBaseline`.
- `apply_rebaseline`: accepts only `Rebaseline`, guard `RebaselineForward`.

Do not reuse `idempotency_decision` unchanged for rebaseline. It currently returns `BaselineRequired` when an installed corpus is behind the media package. Rebaseline needs "behind is acceptable, because this package is full."

Also consider failure cleanup. `apply_baseline` currently creates a building generation and then can fail later, leaving a building registry row/schema for manual cleanup. P5 can keep that behavior for the vertical slice, but it is more visible with rebaseline retries. At minimum, document it and ensure `next_generation_counter` or deterministic generation handling will not silently reuse a half-built schema.

## Answers in Short

1. **Sequence/cursor:** GO. Same lineage, `N -> N+1`, forward-supersession is correct for a self-contained rebaseline. It does not violate gap-free incremental semantics. Fix generation-label compatibility before claiming fresh-client rebaseline catch-up works.

2. **Activation API:** GO. Use `CursorGuard`; one switch implementation is safer than duplicating `activate_generation`. The `FOR UPDATE` read is race-safe under the apply advisory lock. Optionally recheck idempotency under the lock for duplicate same-package applies.

3. **Retired cleanup:** GO with wording change. `DROP SCHEMA ... CASCADE` is acceptable only for registry-confirmed retired private generation cleanup, never for the switch/apply path. The current source already uses it this way.

4. **Schema DDL:** GO. P5 can defer artifact-carried migrations. Current syncd requires clients to be at the package schema version and matching bundle digest.

5. **Isolation:** ADJUST. Core generation/app/control isolation is right, but global `index_manifest` is a P5 correctness gap. Do not update global dense metadata before activation; preferably make it generation/corpus-scoped later.

## Rework Triggers If Built As Proposed

1. Fresh client applies rebaseline with local `core_g0001`, later incremental expects producer `core_g00NN` and fails `active_generation` precondition.
2. Rebaseline writes global `index_manifest` before activation, so old active generation or another corpus can see new dense metadata.
3. `apply_baseline` helpers stay private and P5 duplicates media-apply logic, creating two divergent validation/switch paths.
4. Acceptance literally forbids `DROP SCHEMA ... CASCADE` outside disaster recovery while current cleanup already uses it for retired schemas.

Fix those and the P5 architecture is consistent with the committed P0-P4 code.
