# Review - 02-auto-update-server-crons.md (round 5)

## Findings

No findings.

## Re-check Notes

The round-4 WARN is resolved. The document now states a single `state_dir/update-core.lock` is acquired before ingest and held through enrich, embed, and `producer_cycle("core")` in Part C (`work/10-next-plans/02-auto-update-server-crons.md:225-231`) and Phase 3 (`work/10-next-plans/02-auto-update-server-crons.md:373-388`). Phase 3 now has one lock-acquisition contract: scheduled/manual runs use a bounded wait, proceed after the current workflow when the wait succeeds, and only record `skipped-lock-held` on wait timeout (`work/10-next-plans/02-auto-update-server-crons.md:377-382`). The scheduler invariant now matches that contract: a held jurisprudence run blocks the legislation run from publishing half-processed scopes until the jurisprudence workflow completes (`work/10-next-plans/02-auto-update-server-crons.md:396-401`). Phase 4 also includes `skipped-lock-held` in the classified exit/status taxonomy (`work/10-next-plans/02-auto-update-server-crons.md:414-420`).

The round-4 NIT is resolved. Phase 2 no longer describes a narrow package lock; it names `state_dir/update-core.lock`, says it is acquired before ingest, and explicitly demotes the package builder's per-corpus lock to an internal build-snapshot safeguard (`work/10-next-plans/02-auto-update-server-crons.md:329-333`).

I re-verified the lock/scheduler model against the repository source. The plan's single-`core` assumption still matches source: `KNOWN_SOURCES` maps `legi`, `cass`, `capp`, `inca`, and `jade` to `core` (`crates/jurisearch-package/src/corpus.rs:69-80`), and both LEGI and jurisprudence projections attribute outbox rows through `corpus_for_source()` in the same mutation transaction (`crates/jurisearch-storage/src/projection/legi.rs:209-230`, `crates/jurisearch-storage/src/projection/decisions.rs:159-178`). The package builder still has no completed-run/source-group eligibility boundary: `build_incremental()` only serializes/fences the build snapshot (`crates/jurisearch-package-build/src/incremental.rs:97-125`), takes its window from the latest catalog row and current global change-seq high-water mark (`crates/jurisearch-package-build/src/incremental.rs:139-183`), and `scopes_changed_for_corpus_with_client()` filters only by `corpus` plus `change_seq` range (`crates/jurisearch-storage/src/outbox.rs:222-256`). That supports the document's conclusion that publish-only locking would be unsafe and that v1 needs one workflow lock around all DB-mutating phases.

The surrounding plan remains consistent with source. `producer_cycle()` builds an incremental if the outbox window is non-empty, but always rebuilds and publishes the signed remote manifest afterward (`crates/jurisearch-package-build/src/cycle.rs:67-93`), matching Part C, Phase 2, and the test matrix (`work/10-next-plans/02-auto-update-server-crons.md:219-224`, `346-348`, `496`). Archive selection is still timestamp/name based through `ArchiveSyncFilter` and `select_archives_to_process()`, not package `change_seq` (`crates/jurisearch-cli/src/ingest.rs:327-350`), matching the three-cursor model (`work/10-next-plans/02-auto-update-server-crons.md:334-344`). The producer DB shape remains `ManagedPostgres` rooted at an `index_dir` (`crates/jurisearch-storage/src/runtime.rs:163-180`, `crates/jurisearch-cli/src/index_runtime.rs:39-42`), and the package CLI still exposes build/publish/publish-manifest/list/verify rather than a scheduled producer update command (`crates/jurisearch-package-build/src/bin/jurisearch_package.rs:26-60`), matching the documented implementation gap.

The current document is sound and self-consistent across Part C, Phase 2, Phase 3, Phase 4, and the test matrix.

VERDICT: GO
