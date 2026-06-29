# Review - 02-auto-update-server-crons.md (round 2)

## Findings

### BLOCKER - The plan splits `core` and `jurisprudence` as package corpora, but the current repository attributes all DILA sources to `core`

The revised plan now models `core` as only `LEGI` and adds a separate package/update path for `jurisprudence` (`work/10-next-plans/02-auto-update-server-crons.md:26-28`, `184-191`, `235-244`, `335-357`). That does not match the package/outbox model in source. `corpus_for_source()` maps `legi`, `cass`, `capp`, `inca`, and `jade` all to `core` (`crates/jurisearch-package/src/corpus.rs:69-80`). Both LEGI and jurisprudence projections call that mapping before emitting outbox rows (`crates/jurisearch-storage/src/projection/legi.rs:209-230`, `crates/jurisearch-storage/src/projection/decisions.rs:159-178`). The incremental builder then reads only rows for the requested package corpus and requires a cataloged baseline for that same corpus (`crates/jurisearch-package-build/src/incremental.rs:139-144`, `183-191`; `crates/jurisearch-storage/src/outbox.rs:243-256`).

So after ingesting a CASS/CAPP/INCA/JADE archive, the changed scopes are still `core` changes. A `jurisearch-producer update --corpus jurisprudence` / `producer_cycle("jurisprudence")` would not publish those changes into a jurisprudence chain; depending on setup it would either fail because there is no `jurisprudence` baseline/catalog row or refresh an unrelated empty manifest. The only current package chain that can consume jurisprudence ingest is `core`.

Recommended fix: for v1, separate "source/update groups" from "package corpora". Keep the daily LEGI and daily jurisprudence fetch/ingest cadences if desired, but have both feed the existing `core` package corpus and run `producer_cycle("core")` under a single package lock/manifest. Alternatively, make a true split-corpus design an explicit prerequisite: change `KNOWN_SOURCES` and the storage SQL backfill to map `cass`/`capp`/`inca`/`jade` to `jurisprudence`, create/publish a `jurisprudence` baseline and catalog row, run sites with both corpora, and add regression tests proving juri outbox rows are attributed to and packaged as `jurisprudence`.

## Re-check Notes

The prior round's other findings are resolved in the document:

- The producer DB model is now `ManagedPostgres` rooted at `index_dir`, matching the ingest and package CLIs.
- Archive fetch/ingest cursors are separated from package `change_seq`, and the regression test requirement now targets archive timestamps rather than package sequence.
- Jurisprudence cadence is no longer treated as uniformly weekly; the plan uses daily source checks/no-ops.
- Empty-outbox behavior now correctly says `producer_cycle()` builds no incremental but still refreshes the signed manifest.
- The crate-boundary/orchestration decision now acknowledges that `jurisearch-cli` is a binary crate and recommends shelling out for v1.
- The existing local `jurisearch sync --since` path is now called out.
- The external-source claims are softened and backed by a sources section.

VERDICT: FIXES_REQUIRED
