# Review - 02-auto-update-server-crons.md (round 4)

## Findings

### WARN - The revised lock scope is fixed, but Phase 3 still contradicts itself on wait-vs-skip behavior

The round-3 safety blocker is resolved: the document now says the `core` update lock is acquired before ingest and held through enrich, embed, and `producer_cycle("core")` (`work/10-next-plans/02-auto-update-server-crons.md:225-231`, `371-381`), and the test matrix includes the held-jurisprudence-run race (`work/10-next-plans/02-auto-update-server-crons.md:390-394`, `491`).

However, the scheduler semantics are still internally inconsistent. Phase 3 says that if `state_dir/update-core.lock` is already held, "the next firing exit[s] 0 with `previous run in progress`" (`work/10-next-plans/02-auto-update-server-crons.md:373-375`), but the invariant immediately below says the concurrent legislation timer "must block on the `core` update lock" while the jurisprudence run is held (`work/10-next-plans/02-auto-update-server-crons.md:392-394`). Those are different behaviors. Exiting 0 is safe from the half-processed-publish bug, but it can silently drop that timer's ingest/publish work until the next daily firing, which weakens the stated "publishes whenever LEGI drops or any of CASS/CAPP/INCA/JADE drops" outcome (`work/10-next-plans/02-auto-update-server-crons.md:398-400`).

Recommended fix: choose one lock-acquisition contract and align the deliverable, invariant, and status/exit-code expectations. Prefer a bounded wait on `update-core.lock` for scheduled/manual update runs, so a closely spaced timer runs after the current workflow finishes and still publishes the fetched source group. If the intended behavior is non-blocking skip, make that explicit in the invariant, record a `skipped-lock-held` run/status distinct from a true no-op, and add a near-term catch-up retry so a normal overlap does not add up to a day of latency.

### NIT - Phase 2 still names a `core` package lock where the new model needs an update lock

Phase 2 says to wire `producer_cycle("core")` under a single "`core` package lock" (`work/10-next-plans/02-auto-update-server-crons.md:329-330`). Read together with Part C and Phase 3, the intended lock is now the broader `update-core.lock` that starts before the first DB-mutating step, not a lock around `producer_cycle()` or packaging alone. The wording is easy to implement incorrectly in the shell-out v1 path.

Recommended fix: rewrite this bullet as "run the DB-mutating part of `update` under `state_dir/update-core.lock`, acquired before ingest and held through `producer_cycle("core")`; rely on the existing package builder's per-corpus build lock only as an internal build safeguard." That keeps Phase 2 aligned with the fixed lock model.

## Re-check Notes

The original round-3 blocker is genuinely resolved in the revised design. The current source still has no completed-run/source eligibility boundary in the builder: all five DILA sources map to `core` (`crates/jurisearch-package/src/corpus.rs:75-81`), LEGI and decision projection emit outbox rows in the same mutation transaction using `corpus_for_source()` (`crates/jurisearch-storage/src/projection/legi.rs:212-230`, `crates/jurisearch-storage/src/projection/decisions.rs:159-178`), and `scopes_changed_for_corpus_with_client()` selects only by `corpus` plus global `change_seq` range (`crates/jurisearch-storage/src/outbox.rs:243-257`). `build_incremental()` has a per-corpus build lock and an outbox fence, but those only serialize/freeze the package snapshot; they do not isolate the caller's preceding ingest/enrich/embed workflow (`crates/jurisearch-package-build/src/incremental.rs:107-125`, `175-191`). `producer_cycle()` also takes only `corpus` and publishes whatever that corpus window contains, refreshing the remote manifest even when no incremental is built (`crates/jurisearch-package-build/src/cycle.rs:67-93`).

The document now matches that source-level behavior in the important places: fetch groups are not package corpora, `[package] corpus = "core"` is explicit, the out-of-scope alternative is completed-run/source eligibility in the outbox, and the test matrix covers the half-processed publish race.

VERDICT: FIXES_REQUIRED
