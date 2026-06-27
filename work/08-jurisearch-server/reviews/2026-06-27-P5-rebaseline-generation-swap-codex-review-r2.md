# P5 Code Review Round 2: Re-baseline and Generation Swap

## Findings

No findings.

## Verification Notes

- The r1 blocker is addressed in the media apply path. `apply_media_package` opens one client session, takes `acquire_corpus_apply_lock` for the manifest corpus, runs the locked body, and releases the lock after the body returns. The locked body re-checks idempotency before cleanup/build/switch, then performs `reset_building_generation` -> `create_generation_load_tables` -> payload load -> index build -> validation -> activation while that per-corpus session lock remains held. A second media apply for the same corpus will block before it can observe or reset the first apply's `building` generation; after the first apply commits, the second apply re-enters idempotency and returns `AlreadyApplied` rather than resetting the now-active generation.
- `reset_building_generation` now performs the registry lookup with `FOR UPDATE`, `DROP SCHEMA IF EXISTS ... CASCADE`, registry delete, and commit in a single transaction, and it refuses any row whose state is not `building`. In the apply path, that helper is only reached after the per-corpus apply lock is held, so a concurrent same-corpus media apply can no longer have its in-progress generation dropped by retry cleanup.
- The unlock path is acceptable for normal error returns: the `apply_media_locked` result is captured, `release_corpus_apply_lock` is called before returning it, and the session would also release the session-scoped lock when dropped if the explicit unlock failed.
- The dense-isolation claim has been narrowed rather than over-claimed. The implementation plan now states that generation tables, `corpus_state`, unioned views, and `jurisearch_app` are the proven per-corpus isolation surface, while global `index_manifest` dense-probe metadata remains shared and per-corpus dense metadata is deferred. The loopback test docstring carries the same limitation.
- The deterministic-label false-green hole is covered by ACCEPTANCE 7 in `rebaseline_loopback`: a fresh client with no `core` corpus applies the same re-baseline directly, asserts `corpus_state.active_generation = core_g0002`, and then applies the post-re-baseline incremental. That would fail if `apply_rebaseline` regressed to client-local `next_generation_counter`, because the fresh client would create `core_g0001` and the incremental's signed `active_generation = core_g0002` precondition would reject.

## Validation

- `cargo test -p jurisearch-package-build --test rebaseline_loopback` passed locally.

VERDICT: GO
